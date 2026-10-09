package dev.remoteagent.mobile

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ListQuery
import dev.remoteagent.core.ListSessions
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
) {
    val threads = list?.threads.orEmpty()
    var search by remember { mutableStateOf("") }
    val currentQuery by rememberUpdatedState(query)
    val dispatch by rememberUpdatedState(perform)
    LaunchedEffect(search) {
        kotlinx.coroutines.delay(SEARCH_DEBOUNCE_MILLIS)
        if (currentQuery.searchTerm != search)
            dispatch(
                Intent.ListSessions(ListSessions(query = currentQuery.copy(searchTerm = search)))
            )
    }
    LazyColumn(
        Modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        item { ThreadSearchHeader(search, { search = it }, query, perform, showHosts) }
        list?.notice?.let { notice -> item { Text(notice) } }
        item { Text("プロジェクト", style = MaterialTheme.typography.headlineSmall) }
        projectThreads(list, perform)
        if (list?.hasMoreProjects == true)
            item {
                TextButton(
                    onClick = { perform(Intent.ExpandProjects) },
                    enabled = !loadingThreads,
                ) {
                    LoadingLabel("もっとプロジェクトを表示", loadingThreads)
                }
            }
        item {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text("チャット", style = MaterialTheme.typography.titleMedium)
                TextButton(onClick = { perform(Intent.NewChat("")) }) { Text("新規") }
            }
        }
        items(threads, key = { "chat:${it.id.listKey}" }) { SummaryRow(it, perform) }
        if (list?.hasMore == true)
            item {
                TextButton(
                    onClick = { perform(Intent.ExpandThreadList(null)) },
                    enabled = !loadingThreads,
                ) {
                    LoadingLabel("もっと見る", loadingThreads)
                }
            }
        if (list != null && threads.isEmpty()) item { Text("チャットがありません。") }
    }
}

@Composable
private fun ThreadSearchHeader(
    search: String,
    setSearch: (String) -> Unit,
    query: ListQuery,
    perform: (Intent) -> Unit,
    showHosts: () -> Unit,
) {
    Row {
        Button(onClick = showHosts) { Text("PC一覧") }
        Button(onClick = { perform(Intent.ListSessions(ListSessions(query = query))) }) {
            Text("更新")
        }
    }
    OutlinedTextField(search, setSearch, Modifier.fillMaxWidth(), label = { Text("チャットを検索") })
}

@Composable
private fun LoadingLabel(label: String, loading: Boolean) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (loading) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        Text(label)
    }
}

@Composable
private fun SummaryRow(thread: ThreadSummary, openConversation: (Intent) -> Unit) {
    Row(
        Modifier.fillMaxWidth()
            .clickable { openConversation(Intent.ReadThread(ReadThread(thread.id, open = true))) }
            .padding(vertical = 8.dp)
    ) {
        Text(thread.title, Modifier.weight(1f))
        if (thread.active) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        else if (thread.unread) Text("● 完了・未確認")
        thread.worktreeStatus?.let { status ->
            val unmerged = status == WorktreeStatus.UNMERGED
            Icon(
                painterResource(if (unmerged) R.drawable.ic_diff else R.drawable.ic_merge),
                if (unmerged) "main に未反映の変更あり" else "main にマージ済み",
                Modifier.padding(start = 8.dp).size(18.dp),
                tint =
                    if (unmerged) Color(UNMERGED_COLOR_ARGB) else MaterialTheme.colorScheme.tertiary,
            )
        }
    }
}

private const val SEARCH_DEBOUNCE_MILLIS = 200L
private const val UNMERGED_COLOR_ARGB = 0xFFFB923C

private val SessionRef.listKey: String
    get() = "session:$provider:$id"

private fun LazyListScope.projectThreads(list: ThreadList?, perform: (Intent) -> Unit) {
    val projects = list?.projects.orEmpty()
    projects.forEach { project ->
        item(key = "project:${project.id}") {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Row(
                    Modifier.weight(1f).clickable {
                        perform(Intent.SetProjectExpanded(project.id, !project.expanded))
                    },
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    ProjectIcon(project.iconPng, project.monogram, project.iconColor)
                    Text(project.name, style = MaterialTheme.typography.titleMedium)
                }
                TextButton(
                    onClick = {
                        perform(Intent.NewChat(project.roots.firstOrNull()?.path.orEmpty()))
                    }
                ) {
                    Text("新規")
                }
            }
        }
        if (project.expanded) {
            if (project.loading && !project.hasMore)
                item(key = "loading:${project.id}") {
                    CircularProgressIndicator(Modifier.size(18.dp))
                }
            project.error?.let { message ->
                item(key = "error:${project.id}") {
                    Text(message)
                    TextButton(onClick = { perform(Intent.RefreshProject(project.id)) }) {
                        Text("再試行")
                    }
                }
            }
            items(project.threads, key = { "project:${project.id}:${it.id.listKey}" }) {
                SummaryRow(it, perform)
            }
            if (project.hasMore)
                item(key = "more:${project.id}") {
                    TextButton(
                        onClick = { perform(Intent.ExpandThreadList(project.id)) },
                        enabled = !project.loading,
                    ) {
                        LoadingLabel("もっと見る", project.loading)
                    }
                }
        }
    }
}

@Composable
private fun ProjectIcon(png: ByteArray?, monogram: String, colorRgb: UInt) {
    val bitmap =
        remember(png) {
            png?.let { BitmapFactory.decodeByteArray(it, 0, it.size)?.asImageBitmap() }
        }
    val color = Color(colorRgb.toInt()).copy(alpha = 1f)
    val modifier = Modifier.size(24.dp).clip(RoundedCornerShape(6.dp)).clearAndSetSemantics {}
    if (bitmap != null) {
        Image(bitmap, contentDescription = null, modifier = modifier)
    } else {
        Box(modifier.background(color.copy(alpha = 0.15f)), contentAlignment = Alignment.Center) {
            Text(monogram, color = color, fontSize = 10.sp, fontWeight = FontWeight.Bold)
        }
    }
}

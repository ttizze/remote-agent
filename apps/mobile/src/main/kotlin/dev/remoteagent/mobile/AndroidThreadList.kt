package dev.remoteagent.mobile

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
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
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.painterResource
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ListQuery
import dev.remoteagent.core.ListThreads
import dev.remoteagent.core.ReadThread
import dev.remoteagent.core.ThreadList
import dev.remoteagent.core.ThreadSummary

@Composable
internal fun ThreadListScreen(
    list: ThreadList?,
    query: ListQuery,
    perform: (Intent) -> Unit,
    showHosts: () -> Unit,
    openConversation: (Intent) -> Unit,
    modifier: Modifier = Modifier,
) {
    val threads = list?.threads.orEmpty()
    var search by remember { mutableStateOf("") }
    val currentQuery by rememberUpdatedState(query)
    val dispatch by rememberUpdatedState(perform)
    LaunchedEffect(search) {
        kotlinx.coroutines.delay(SEARCH_DEBOUNCE_MILLIS)
        if (currentQuery.searchTerm != search)
            dispatch(Intent.ListThreads(ListThreads(query = currentQuery.copy(searchTerm = search))))
    }
    LazyColumn(
        modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        item {
            Row {
                Button(onClick = showHosts) { Text("PC一覧") }
                Button(onClick = { perform(Intent.ListThreads(ListThreads(query = query))) }) { Text("更新") }
            }
            OutlinedTextField(search, { search = it }, Modifier.fillMaxWidth(), label = { Text("チャットを検索") })
            Text("プロジェクト", style = MaterialTheme.typography.headlineSmall)
        }
        projectThreads(list, openConversation, perform)
        item {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text("チャット", style = MaterialTheme.typography.titleMedium)
                TextButton(onClick = { openConversation(Intent.NewChat("")) }) { Text("新規") }
            }
        }
        items(threads.filter { it.projectId == null }, key = { it.id }) { SummaryRow(it, openConversation) }
        if (list?.hasMoreChats == true)
            item { TextButton(onClick = { perform(Intent.ExpandThreadList(null, false)) }) { Text("もっと見る") } }
        if (list != null && threads.isEmpty()) item { Text("タスクがありません。") }
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
        if (thread.worktreeMerged) Icon(
            painterResource(R.drawable.ic_merge), "main にマージ済み",
            Modifier.padding(start = 8.dp).size(18.dp),
            tint = MaterialTheme.colorScheme.tertiary,
        )
    }
}

private const val SEARCH_DEBOUNCE_MILLIS = 200L

private fun LazyListScope.projectThreads(
    list: ThreadList?,
    openConversation: (Intent) -> Unit,
    perform: (Intent) -> Unit,
) {
    val projects = list?.projects.orEmpty()
    val threads = list?.threads.orEmpty()
    projects.forEach { project ->
        item(key = "project:${project.id}") {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text("📁 ${project.name}", style = MaterialTheme.typography.titleMedium)
                TextButton(
                    onClick = { openConversation(Intent.NewChat(project.roots.firstOrNull()?.path.orEmpty())) }
                ) {
                    Text("新規")
                }
            }
        }
        items(threads.filter { it.projectId == project.id }, key = { it.id }) { SummaryRow(it, openConversation) }
        if (project.id in list?.moreProjectIds.orEmpty())
            item(key = "more:${project.id}") {
                TextButton(onClick = { perform(Intent.ExpandThreadList(project.id, false)) }) { Text("もっと見る") }
            }
    }
    if (list?.hasMoreProjects == true)
        item { TextButton(onClick = { perform(Intent.ExpandThreadList(null, true)) }) { Text("もっと見る") } }
}

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
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.ListThreads
import dev.remoteagent.core.ReadOlder
import dev.remoteagent.core.ReadThread
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.ThreadSummary
import java.util.UUID

@Composable
internal fun ThreadListScreen(model: AndroidAppModel, modifier: Modifier) {
    val list = model.list
    val projects = list?.projects.orEmpty()
    val threads = list?.threads.orEmpty()
    val known = projects.map { it.id }.toSet()
    var search by remember { mutableStateOf("") }
    LaunchedEffect(search) {
        kotlinx.coroutines.delay(SEARCH_DEBOUNCE_MILLIS)
        if (model.snapshot.listQuery().searchTerm != search)
            model.perform(Intent.ListThreads(ListThreads(query = model.snapshot.listQuery().copy(searchTerm = search))))
    }
    LazyColumn(
        modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        item {
            Row {
                Button(onClick = model::showHosts) { Text("PC一覧") }
                Button(onClick = model::refresh) { Text("更新") }
            }
            OutlinedTextField(search, { search = it }, Modifier.fillMaxWidth(), label = { Text("チャットを検索") })
            Text("プロジェクト", style = MaterialTheme.typography.headlineSmall)
        }
        projects.forEach { project ->
            item(key = "project:${project.id}") {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text("📁 ${project.name}", style = MaterialTheme.typography.titleMedium)
                    TextButton(onClick = { model.newChat(project.roots.firstOrNull()?.path.orEmpty()) }) { Text("新規") }
                }
            }
            items(threads.filter { it.projectId == project.id }, key = { it.id }) { SummaryRow(it, model) }
            if (project.id in list?.moreProjectIds.orEmpty())
                item(key = "more:${project.id}") {
                    TextButton(onClick = { model.expand(projectId = project.id) }) { Text("もっと見る") }
                }
        }
        if (list?.hasMoreProjects == true)
            item { TextButton(onClick = { model.expand(projects = true) }) { Text("もっと見る") } }
        item {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text("チャット", style = MaterialTheme.typography.titleMedium)
                TextButton(onClick = { model.newChat("") }) { Text("新規") }
            }
        }
        items(threads.filter { it.projectId !in known }, key = { it.id }) { SummaryRow(it, model) }
        if (list?.hasMoreChats == true) item { TextButton(onClick = { model.expand() }) { Text("もっと見る") } }
        if (list != null && threads.isEmpty()) item { Text("タスクがありません。") }
    }
}

@Composable
private fun SummaryRow(thread: ThreadSummary, model: AndroidAppModel) {
    Row(Modifier.fillMaxWidth().clickable { model.openThread(thread.id) }.padding(vertical = 8.dp)) {
        Text(thread.name.ifEmpty { thread.preview.ifEmpty { "無題のタスク" } }, Modifier.weight(1f))
        if (thread.active) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        else if (thread.unread) Text("● 完了・未確認")
    }
}

internal fun AndroidAppModel.refresh() {
    perform(Intent.ListThreads(ListThreads(query = snapshot.listQuery())))
}

internal fun AndroidAppModel.expand(projects: Boolean = false, projectId: String? = null) {
    val query = snapshot.listQuery()
    val expanded =
        when {
            projectId != null ->
                query.copy(
                    projectThreadLimits =
                        query.projectThreadLimits +
                            (projectId to
                                ((query.projectThreadLimits[projectId] ?: THREAD_PAGE_SIZE) + THREAD_PAGE_INCREMENT))
                )
            projects -> query.copy(projectLimit = query.projectLimit + THREAD_PAGE_INCREMENT)
            else -> query.copy(chatLimit = query.chatLimit + THREAD_PAGE_INCREMENT)
        }
    perform(Intent.ListThreads(ListThreads(query = expanded)))
}

internal fun AndroidAppModel.showThreads() {
    screen = Screen.Threads
    perform(Intent.ShowThreadList)
    refresh()
}

internal fun AndroidAppModel.showHosts() {
    screen = Screen.Hosts
    perform(Intent.ShowThreadList)
}

internal fun AndroidAppModel.openThread(id: String) {
    screen = Screen.Conversation
    perform(Intent.ReadThread(ReadThread(id, open = true)))
}

internal fun AndroidAppModel.newChat(cwd: String) {
    screen = Screen.Conversation
    perform(Intent.NewChat(cwd))
}

internal fun AndroidAppModel.older(turnId: String?) {
    val id = snapshot.navigation().threadId ?: return
    val thread = snapshot.conversation(id) ?: return
    val cursor =
        if (turnId == null) thread.historyCursor() else thread.turns().firstOrNull { it.id() == turnId }?.itemsCursor()
    loadingHistory = true
    perform(Intent.ReadOlder(ReadOlder(id, turnId, cursor))) { loadingHistory = false }
}

internal fun AndroidAppModel.send(complete: (Result<Outcome>) -> Unit) {
    perform(Intent.Submit(snapshot.navigation().threadId, UUID.randomUUID().toString()), complete)
}

private const val THREAD_PAGE_SIZE = 5u

private const val THREAD_PAGE_INCREMENT = 10u

private const val SEARCH_DEBOUNCE_MILLIS = 200L

package dev.remoteagent.mobile

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.atomicfu.AtomicRef
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

@Composable
internal fun ThreadListScreen(
    state: AppState,
    controller: AtomicRef<MobileApp>,
    scope: CoroutineScope,
    modifier: Modifier,
) {
    val profile = requireNotNull(state.selectedProfile)
    val view = state.selectedView
    val cache = state.cache.profile(profile.id)
    val projects = cache.projects
    val threads = cache.threadList
    val onRefresh: () -> Unit = { scope.launch { controller.listThreads(profile) } }
    val onNew: (String) -> Unit = { controller.openNewThread(profile, it) }
    val onSelect: (String) -> Unit = { scope.launch { controller.readThread(profile, it) } }
    val onExpand: (Boolean, String?) -> Unit = { expandProjects, projectId ->
        scope.launch { controller.expandTaskList(profile, expandProjects, projectId) }
    }
    val knownProjectIds = projects.mapTo(mutableSetOf()) { it.id }
    val unassigned = threads.filter { it.projectId == null || it.projectId !in knownProjectIds }
    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        item { ThreadListHeader(profile, view, onRefresh) }
        projects.take(view.visibleProjectCount).forEach { project ->
            item(key = "project-${project.id}") { ProjectHeader(project, onNew) }
            items(threads.filter { it.projectId == project.id }, key = { it.id }) { thread ->
                ThreadSummaryRow(thread, onSelect)
            }
            if (project.id in view.moreProjectIds) {
                item(key = "project-${project.id}-more") {
                    TextButton(onClick = { onExpand(false, project.id) }, enabled = !view.loadingMoreThreads) {
                        Text("もっと見る")
                    }
                }
            }
        }
        if (view.hasMoreProjects) {
            item(key = "projects-more") {
                TextButton(onClick = { onExpand(true, null) }, enabled = !view.loadingMoreThreads) { Text("もっと見る") }
            }
        }
        item(key = "unassigned-header") {
            Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("チャット", style = MaterialTheme.typography.titleMedium)
                Spacer(Modifier.weight(1f))
                TextButton(onClick = { onNew("") }) { Text("新規") }
            }
        }
        items(unassigned, key = { it.id }) { thread -> ThreadSummaryRow(thread, onSelect) }
        if (view.hasMoreChats) {
            item(key = "chats-more") {
                TextButton(onClick = { onExpand(false, null) }, enabled = !view.loadingMoreThreads) { Text("もっと見る") }
            }
        }
        if (view.threadList == LoadPhase.Ready && threads.isEmpty()) {
            item { Text("タスクがありません。", color = MaterialTheme.colorScheme.onSurfaceVariant) }
        }
    }
}

@Composable
private fun ThreadSummaryRow(thread: ThreadSummary, onSelect: (String) -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().clickable(onClick = { onSelect(thread.id) }).padding(vertical = 8.dp),
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
    ) {
        Text(thread.name ?: thread.preview.ifBlank { "無題のタスク" }, modifier = Modifier.weight(1f))
        if (thread.status is ThreadStatus.Active) {
            CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
        }
    }
}

@Composable
private fun ThreadListHeader(profile: HostProfile, view: ProfileViewState, onRefresh: () -> Unit) {
    Text("プロジェクト", style = MaterialTheme.typography.headlineSmall)
    Text(profile.name, style = MaterialTheme.typography.bodyMedium)
    Button(onClick = onRefresh, enabled = view.threadList != LoadPhase.Loading) { Text("更新") }
    view.notice?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    when (val phase = view.threadList) {
        LoadPhase.Idle -> Text("Codexのプロジェクトとタスクを読み込みます。")
        LoadPhase.Loading -> Text("プロジェクトとタスクを読み込み中…")
        LoadPhase.Ready -> Unit
        is LoadPhase.Failed -> {
            Text(phase.message, color = MaterialTheme.colorScheme.error)
            Button(onClick = onRefresh) { Text("再試行") }
        }
    }
}

@Composable
private fun ProjectHeader(project: CodexProject, onNew: (String) -> Unit) {
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("📁 ${project.name}", style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.weight(1f))
        TextButton(onClick = { onNew(project.roots.firstOrNull()?.path.orEmpty()) }) { Text("新規") }
    }
}

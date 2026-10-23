package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ReadThread
import dev.remoteagent.core.SessionRef

@Composable
internal fun ThreadListHeader(
    search: String?,
    loadingThreads: Boolean,
    changeSearch: (String?) -> Unit,
    refresh: () -> Unit,
    title: @Composable RowScope.() -> Unit,
) {
    Row(Modifier.fillMaxWidth().padding(bottom = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        if (search != null) {
            val focusRequester = remember { FocusRequester() }
            LaunchedEffect(Unit) { focusRequester.requestFocus() }
            OutlinedTextField(
                value = search,
                onValueChange = changeSearch,
                modifier = Modifier.weight(1f).focusRequester(focusRequester),
                placeholder = { Text("チャットを検索", fontSize = 14.sp) },
                singleLine = true,
                shape = RoundedCornerShape(28.dp),
            )
            IconButton(onClick = { changeSearch(null) }) { Icon(painterResource(R.drawable.ic_close), "検索を閉じる") }
        } else {
            title()
            IconButton(onClick = { changeSearch("") }) { Icon(painterResource(R.drawable.ic_search), "検索") }
            IconButton(onClick = refresh, enabled = !loadingThreads) {
                Icon(painterResource(R.drawable.ic_refresh), "更新")
            }
        }
    }
}

@Composable
internal fun CoordinatorEntry(session: SessionRef, perform: (Intent) -> Unit) {
    TextButton(
        onClick = { perform(Intent.ReadThread(ReadThread(session, open = true))) },
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text("メイン会話を開く")
    }
    Text(
        "タスクの依頼・振り分けはメイン会話へ。以下はこのPCの作業スレッドです。",
        Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

package dev.remoteagent.mobile

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.LoadModels
import dev.remoteagent.core.ModelRef
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.ProviderKind
import dev.remoteagent.core.Snapshot

@Composable
internal fun ModelPickerButton(
    snapshot: Snapshot,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    modifier: Modifier = Modifier,
) {
    val key = snapshot.navigation().draftKey
    val draft = snapshot.draft(key)
    val provider = snapshot.modelProviderForDraft(key)
    val selected = draft.model ?: provider?.let { snapshot.modelForProvider(key, it) }
    val model = snapshot.models().firstOrNull { it.model == selected }
    val controls = snapshot.modelQuickControls(key)
    var open by remember(key) { mutableStateOf(false) }
    TextButton(
        onClick = {
            open = true
            perform(Intent.LoadModels(LoadModels())) {}
        },
        modifier = modifier.testTag("model.settings"),
    ) {
        Text(
            listOfNotNull(
                    if (controls.fast) "⚡" else null,
                    model?.displayName ?: selected?.id ?: "モデルを選択",
                    controls.effort.takeIf { it.isNotEmpty() }?.let(::effortLabel),
                )
                .joinToString(" "),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
    if (open) ModelPickerDialog(snapshot, selected, perform) { open = false }
}

@Composable
private fun ModelPickerDialog(
    snapshot: Snapshot,
    selected: ModelRef?,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    dismiss: () -> Unit,
) {
    val key = snapshot.navigation().draftKey
    var query by remember(key) { mutableStateOf("") }
    val fixedProvider = snapshot.navigation().threadId?.provider
    val choices = snapshot.modelsMatching(fixedProvider, query)
    AlertDialog(
        onDismissRequest = { dismiss() },
        title = { Text("モデルと実行設定") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    query,
                    { query = it },
                    label = { Text("モデルを検索") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                if (fixedProvider != null)
                    Text("この会話のAIは固定されています。別のAIは新規チャットで選べます。", style = MaterialTheme.typography.bodySmall)
                LazyColumn(Modifier.heightIn(max = 220.dp)) {
                    items(choices, key = { "${it.model.provider}:${it.model.id}" }) { choice ->
                        TextButton(
                            onClick = { perform(Intent.SelectModel(key, choice.model)) {} },
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Column(Modifier.fillMaxWidth().padding(vertical = 2.dp)) {
                                Text("${if (choice.model == selected) "✓ " else ""}${choice.displayName}")
                                Text(
                                    if (choice.model.provider == ProviderKind.CODEX) "Codex" else "Claude",
                                    style = MaterialTheme.typography.labelSmall,
                                )
                            }
                        }
                    }
                }
                if (choices.isEmpty()) Text("モデルがありません。検索条件やPCとの接続を確認してください。")
                ModelQuickSettings(snapshot, perform)
            }
        },
        confirmButton = { TextButton(onClick = { dismiss() }) { Text("完了") } },
    )
}

@Composable
private fun ModelQuickSettings(snapshot: Snapshot, perform: (Intent, (Result<Outcome>) -> Unit) -> Unit) {
    val key = snapshot.navigation().draftKey
    val controls = snapshot.modelQuickControls(key)
    if (controls.efforts.isNotEmpty()) {
        Text("推論量", style = MaterialTheme.typography.labelLarge)
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            controls.efforts.forEach { effort ->
                FilterChip(
                    selected = controls.effort == effort,
                    onClick = { perform(Intent.SelectEffort(key, effort)) {} },
                    label = { Text(effortLabel(effort)) },
                )
            }
        }
    }
    if (controls.speedChoices.isNotEmpty()) {
        Text("速度", style = MaterialTheme.typography.labelLarge)
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            controls.speedChoices.forEach { speed ->
                FilterChip(
                    selected = speed.selected,
                    onClick = { perform(Intent.SelectServiceTier(key, speed.id)) {} },
                    label = { Text(speed.label) },
                )
            }
        }
        controls.speedChoices
            .firstOrNull { it.selected }
            ?.description
            ?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
    }
}

private fun effortLabel(value: String): String =
    when (value) {
        "low" -> "低"
        "medium" -> "中"
        "high" -> "高"
        "xhigh" -> "最高"
        "max" -> "最大"
        "minimal" -> "最小"
        "none" -> "なし"
        else -> value
    }

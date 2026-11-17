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
import dev.remoteagent.core.ComposerPresentation
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ModelRef
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.ProviderKind
import dev.remoteagent.core.SettingsPresentation

@Composable
internal fun ModelPickerButton(
    composer: ComposerPresentation,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    modifier: Modifier = Modifier,
    dialog: @Composable (ModelRef?, () -> Unit) -> Unit,
) {
    val language = LocalUiLanguage.current
    val controls = composer.controls()
    val key = controls.navigation.draftKey
    val selected = controls.selectedModel
    val model = composer.currentModel()
    val quick = controls.quick
    var open by remember(key) { mutableStateOf(false) }
    TextButton(
        onClick = {
            open = true
            perform(Intent.RefreshModels) {}
        },
        modifier = modifier.testTag("model.settings"),
        enabled = composer.canChooseModel(),
    ) {
        Text(
            listOfNotNull(
                    if (quick.fast) "⚡" else null,
                    model?.displayName ?: selected?.id ?: language.text("モデルを選択"),
                    quick.effort.takeIf { it.isNotEmpty() }?.let(::effortLabel),
                )
                .joinToString(" "),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
    if (open) dialog(selected) { open = false }
}

@Composable
internal fun ModelPickerDialog(
    settings: SettingsPresentation,
    selected: ModelRef?,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    dismiss: () -> Unit,
) {
    val language = LocalUiLanguage.current
    val key = settings.draftKey()
    var query by remember(key) { mutableStateOf("") }
    val choices = settings.modelsMatching(null, query)
    AlertDialog(
        onDismissRequest = { dismiss() },
        title = { Text(language.text("モデルと実行設定")) },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                ErrorNotices(settings.notices(), perform)
                OutlinedTextField(
                    query,
                    { query = it },
                    label = { Text(language.text("モデルを検索")) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                LazyColumn(Modifier.heightIn(max = 220.dp)) {
                    items(choices, key = { "${it.model.provider}:${it.model.id}" }) { choice ->
                        TextButton(
                            onClick = { perform(Intent.SelectModel(key, choice.model)) {} },
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Column(Modifier.fillMaxWidth().padding(vertical = 2.dp)) {
                                Text("${if (choice.model == selected) "✓ " else ""}${choice.displayName}")
                                Text(
                                    when (choice.model.provider) {
                                        ProviderKind.CODEX -> "Codex"
                                        ProviderKind.CLAUDE -> "Claude"
                                        ProviderKind.DEEP_SEEK -> "DeepSeek"
                                    },
                                    style = MaterialTheme.typography.labelSmall,
                                )
                            }
                        }
                    }
                }
                if (choices.isEmpty()) {
                    Text(
                        language.text(
                            if (settings.modelsLoading(null)) "モデルを読み込み中…" else "モデルがありません。検索条件やPCとの接続を確認してください。"
                        )
                    )
                }
                ModelQuickSettings(settings, perform)
                if (selected?.provider == dev.remoteagent.core.ProviderKind.DEEP_SEEK)
                    CredentialSettings(settings, perform)
            }
        },
        confirmButton = { TextButton(onClick = { dismiss() }) { Text(language.text("完了")) } },
    )
}

@Composable
private fun ModelQuickSettings(settings: SettingsPresentation, perform: (Intent, (Result<Outcome>) -> Unit) -> Unit) {
    val language = LocalUiLanguage.current
    val key = settings.draftKey()
    val controls = settings.quickControls()
    if (controls.efforts.isNotEmpty()) {
        Text(language.text("推論量"), style = MaterialTheme.typography.labelLarge)
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            controls.efforts.forEach { effort ->
                FilterChip(
                    selected = controls.effort == effort,
                    onClick = { perform(Intent.SelectEffort(key, effort)) {} },
                    label = { Text(LocalUiLanguage.current.text(effortLabel(effort))) },
                )
            }
        }
    }
    if (controls.speedChoices.isNotEmpty()) {
        Text(language.text("速度"), style = MaterialTheme.typography.labelLarge)
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
        "off" -> "オフ"
        "" -> "既定"
        else -> value
    }

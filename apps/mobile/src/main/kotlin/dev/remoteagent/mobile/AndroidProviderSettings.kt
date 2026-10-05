package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.ApplyProviderEdit
import dev.remoteagent.core.ConfiguredProvider
import dev.remoteagent.core.EnvironmentVariable
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ProviderEditor
import dev.remoteagent.core.ProviderFieldKind
import dev.remoteagent.core.ReadProviderSettings

@Composable
internal fun ProviderSettings(model: AndroidAppModel, close: () -> Unit) {
    var editor by remember(model.profileId) { mutableStateOf<ProviderEditor?>(null) }
    var busy by remember(model.profileId) { mutableStateOf(false) }
    var error by remember(model.profileId) { mutableStateOf<String?>(null) }
    fun load() {
        busy = true
        error = null
        model.perform(Intent.ReadProviderSettings(ReadProviderSettings())) { result ->
            busy = false
            error = result.exceptionOrNull()?.message
        }
    }
    LaunchedEffect(model.profileId) { load() }
    Column(
        Modifier.verticalScroll(rememberScrollState()).padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            TextButton(onClick = { if (editor == null) close() else editor = null }, enabled = !busy) { Text("戻る") }
            Text("Providers", style = MaterialTheme.typography.headlineSmall)
        }
        val current = editor
        if (current == null) {
            key(model.profileId) {
                ProviderInstanceList(
                    model.snapshot.providerSettings()?.instances.orEmpty(),
                    !busy && model.snapshot.providerSettings() != null && model.snapshot.connected(),
                ) { id, driver ->
                    editor = model.snapshot.providerEditor(id, driver)
                    if (editor == null) error = "Driver IDを確認してください。"
                }
            }
        } else {
            ProviderEditorForm(current, !busy && model.snapshot.connected(), { editor = it }) { remove ->
                busy = true
                error = null
                model.perform(Intent.ApplyProviderEdit(ApplyProviderEdit(current, remove))) { result ->
                    busy = false
                    error = result.exceptionOrNull()?.message
                    if (result.isSuccess) editor = null
                }
            }
        }
        if (busy) CircularProgressIndicator()
        error?.let { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.testTag("provider.error")) }
        TextButton(
            onClick = {
                editor = null
                load()
            },
            enabled = !busy && model.snapshot.connected(),
        ) {
            Text("再読み込み")
        }
    }
}

@Composable
private fun ProviderInstanceList(
    instances: List<ConfiguredProvider>,
    enabled: Boolean,
    edit: (String?, String) -> Unit,
) {
    var driver by remember { mutableStateOf("codex") }
    for (instance in instances) {
        TextButton(
            onClick = { edit(instance.instanceId, "") },
            enabled = enabled,
            modifier = Modifier.testTag("provider.edit.${instance.instanceId}"),
        ) {
            Column {
                Text(instance.config.displayName ?: instance.instanceId)
                Text(
                    "${instance.instanceId} · ${instance.config.driver}${if (instance.config.enabled) "" else " · 無効"}"
                )
            }
        }
    }
    ProviderTextField("Driver ID", driver, enabled) { driver = it }
    Button(onClick = { edit(null, driver) }, enabled = enabled, modifier = Modifier.testTag("provider.add")) {
        Text("Providerを追加")
    }
}

@Composable
private fun ProviderEditorForm(
    editor: ProviderEditor,
    enabled: Boolean,
    change: (ProviderEditor) -> Unit,
    save: (Boolean) -> Unit,
) {
    var confirmRemoval by remember { mutableStateOf(false) }
    ProviderTextField("ID", editor.instanceId, enabled && editor.existingId == null, "provider.id") {
        change(editor.copy(instanceId = it))
    }
    Text("Driver: ${editor.driver}")
    ProviderTextField("表示名", editor.displayName, enabled, "provider.name") { change(editor.copy(displayName = it)) }
    ProviderTextField("アクセントカラー", editor.accentColor, enabled) { change(editor.copy(accentColor = it)) }
    ProviderToggle("有効", editor.enabled, enabled) { change(editor.copy(enabled = it)) }
    for ((index, field) in editor.fields.withIndex()) {
        fun setValue(value: String) =
            change(
                editor.copy(
                    fields = editor.fields.mapIndexed { i, item -> if (i == index) item.copy(value = value) else item }
                )
            )
        ProviderTextField(
            field.label,
            field.value,
            enabled,
            "provider.config.${field.key}",
            field.kind == ProviderFieldKind.JSON,
            ::setValue,
        )
    }
    if (editor.fields.isEmpty())
        ProviderTextField("Driver設定のJSON", editor.configJson, enabled, multiline = true) {
            change(editor.copy(configJson = it))
        }
    ProviderEnvironment(editor, enabled, change)
    Button(onClick = { save(false) }, enabled = enabled, modifier = Modifier.testTag("provider.save")) { Text("保存") }
    if (editor.existingId != null)
        TextButton(onClick = { confirmRemoval = true }, enabled = enabled) { Text("Providerを削除") }
    if (confirmRemoval) {
        AlertDialog(
            onDismissRequest = { confirmRemoval = false },
            title = { Text("このproviderを削除しますか？") },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmRemoval = false
                        save(true)
                    }
                ) {
                    Text("削除")
                }
            },
            dismissButton = { TextButton(onClick = { confirmRemoval = false }) { Text("キャンセル") } },
        )
    }
}

@Composable
private fun ProviderEnvironment(editor: ProviderEditor, enabled: Boolean, change: (ProviderEditor) -> Unit) {
    Text("環境変数", style = MaterialTheme.typography.titleMedium)
    for ((index, variable) in editor.environment.withIndex()) {
        fun update(value: EnvironmentVariable) =
            change(
                editor.copy(environment = editor.environment.mapIndexed { i, item -> if (i == index) value else item })
            )
        ProviderTextField("名前", variable.name, enabled) { update(variable.copy(name = it)) }
        OutlinedTextField(
            value = variable.value,
            onValueChange = { update(variable.copy(value = it, valueRedacted = false)) },
            label = { Text(if (variable.valueRedacted) "保存済み（変更する場合だけ入力）" else "値") },
            enabled = enabled,
            visualTransformation =
                if (variable.sensitive) PasswordVisualTransformation() else VisualTransformation.None,
            keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
            modifier = Modifier.fillMaxWidth(),
        )
        ProviderToggle("秘密値", variable.sensitive, enabled) { update(variable.copy(sensitive = it)) }
        TextButton(
            onClick = { change(editor.copy(environment = editor.environment.filterIndexed { i, _ -> i != index })) },
            enabled = enabled,
        ) {
            Text("変数を削除")
        }
    }
    TextButton(
        onClick = { change(editor.copy(environment = editor.environment + EnvironmentVariable("", "", false, false))) },
        enabled = enabled,
    ) {
        Text("環境変数を追加")
    }
}

@Composable
private fun ProviderToggle(label: String, checked: Boolean, enabled: Boolean, change: (Boolean) -> Unit) {
    Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(label, modifier = Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = change, enabled = enabled)
    }
}

@Composable
private fun ProviderTextField(
    label: String,
    value: String,
    enabled: Boolean,
    tag: String = label,
    multiline: Boolean = false,
    change: (String) -> Unit,
) {
    OutlinedTextField(
        value = value,
        onValueChange = change,
        label = { Text(label) },
        enabled = enabled,
        singleLine = !multiline,
        minLines = if (multiline) 3 else 1,
        keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
        modifier = Modifier.fillMaxWidth().testTag(tag),
    )
}

package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.LoadModels
import dev.remoteagent.core.ModelDefaultsScope
import dev.remoteagent.core.ModelOptionControl
import dev.remoteagent.core.ModelOptionValue
import dev.remoteagent.core.ModelScopeChoice

@Composable
internal fun ModelSettings(model: AndroidAppModel, close: () -> Unit) {
    var scope by remember(model.profileId) { mutableStateOf<ModelDefaultsScope>(ModelDefaultsScope.Global) }
    var loading by remember(model.profileId) { mutableStateOf(false) }
    var error by remember(model.profileId) { mutableStateOf<String?>(null) }
    fun reload() {
        loading = true
        error = null
        model.perform(Intent.LoadModels(LoadModels())) { result ->
            loading = false
            error = result.exceptionOrNull()?.message
        }
    }
    LaunchedEffect(model.profileId) { reload() }
    val snapshot = model.snapshot
    val enabled = snapshot.connected() && !loading
    val defaults = snapshot.modelDefaults(scope)
    val current = snapshot.defaultModel(scope)
    Column(
        Modifier.verticalScroll(rememberScrollState()).padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            TextButton(onClick = close) { Text("戻る") }
            Text("新しい会話", style = MaterialTheme.typography.headlineSmall)
        }
        Text("モデル・推論強度・速度の初期値です。この端末に保存され、会話ごとに変更できます。")
        ModelDefaultsScopes(
            snapshot.modelEnvironmentScopeChoices(scope),
            snapshot.modelProjectScopeChoices(scope),
            scope,
            enabled,
        ) {
            scope = it
        }
        ModelChoiceMenu(
            "モデル: ${defaults.model?.let { current?.displayName ?: it.id } ?: "自動"}",
            listOf("自動" to null) +
                snapshot.modelsMatching(null, "").map {
                    "${snapshot.instanceName(it.model.instanceId)} · ${it.displayName}" to it.model
                },
            defaults.model,
            enabled,
            Modifier.testTag("models.default"),
        ) {
            model.perform(Intent.SelectDefaultModel(scope, it))
        }
        ModelOptionChoices(snapshot.defaultModelOptionControls(scope), enabled, defaults = true) { id, value ->
            model.perform(Intent.SelectDefaultModelOption(scope, id, value))
        }
        if (snapshot.hasModelDefaultsOverride(scope)) {
            TextButton(onClick = { model.perform(Intent.InheritModelDefaults(scope)) }, enabled = enabled) {
                Text("上位の設定を使う")
            }
        }
        ModelCatalogStatus(loading, listOfNotNull(error) + snapshot.modelErrorMessages(null), enabled, ::reload)
    }
}

@Composable
internal fun ModelCatalogStatus(loading: Boolean, errors: List<String>, enabled: Boolean, reload: () -> Unit) {
    if (loading) CircularProgressIndicator()
    for (message in errors) Text(message, color = MaterialTheme.colorScheme.error)
    TextButton(onClick = reload, enabled = enabled) { Text("再読み込み") }
}

@Composable
private fun ModelDefaultsScopes(
    environments: List<ModelScopeChoice>,
    projects: List<ModelScopeChoice>,
    scope: ModelDefaultsScope,
    enabled: Boolean,
    choose: (ModelDefaultsScope) -> Unit,
) {
    ModelChoiceMenu(
        "環境: ${environments.firstOrNull { it.scope == scope }?.label ?: "すべての環境"}",
        environments.map { it.label to it.scope },
        scope,
        enabled,
        Modifier.testTag("models.scope.environment"),
        choose,
    )
    ModelChoiceMenu(
        "プロジェクト: ${projects.firstOrNull { it.scope == scope }?.label ?: "すべてのプロジェクト"}",
        projects.map { it.label to it.scope },
        scope,
        enabled,
        Modifier.testTag("models.scope.project"),
        choose,
    )
}

@Composable
internal fun ModelOptionChoices(
    controls: List<ModelOptionControl>,
    enabled: Boolean,
    defaults: Boolean = false,
    choose: (String, ModelOptionValue?) -> Unit,
) {
    for (control in controls) {
        val choices: List<Pair<String, ModelOptionValue?>> = control.choices.map { it.label to it.value }
        val automatic = defaults && !control.isExplicit
        ModelChoiceMenu(
            "${control.label}: ${if (automatic) "自動" else control.valueLabel ?: "未設定"}",
            (if (defaults) listOf("自動" to null) else emptyList()) + choices,
            if (automatic) null else control.value,
            enabled && control.disabledReason == null,
            Modifier.testTag("model.option.${control.id}"),
        ) {
            choose(control.id, it)
        }
        (control.disabledReason ?: control.description)?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
    }
}

@Composable
internal fun <T> ModelChoiceMenu(
    title: String,
    choices: List<Pair<String, T>>,
    selected: T,
    enabled: Boolean,
    modifier: Modifier = Modifier,
    choose: (T) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        TextButton(onClick = { expanded = true }, enabled = enabled, modifier = modifier) { Text(title) }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            for ((name, choice) in choices) {
                DropdownMenuItem(
                    enabled = enabled,
                    text = { Text(name) },
                    leadingIcon = if (choice == selected) ({ Text("✓") }) else null,
                    onClick = {
                        expanded = false
                        choose(choice)
                    },
                )
            }
        }
    }
}

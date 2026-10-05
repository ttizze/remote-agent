package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
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
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Account
import dev.remoteagent.core.Intent
import dev.remoteagent.core.LoadModels
import dev.remoteagent.core.Model
import dev.remoteagent.core.ModelRef
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.SelectAccountForDraft
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.UsageWindow

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ConversationModelPicker(
    snapshot: Snapshot,
    enabled: Boolean,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
) {
    val key = snapshot.composerDraftKey()
    val draft = snapshot.draft(key)
    var showing by remember(key) { mutableStateOf(false) }
    val current = snapshot.modelsMatching(null, "").firstOrNull { it.model == draft.model }
    TextButton(onClick = { showing = true }, enabled = enabled, modifier = Modifier.testTag("model.open")) {
        Text(current?.displayName ?: draft.model?.id ?: "モデル")
    }
    if (showing) {
        ModalBottomSheet(onDismissRequest = { showing = false }, containerColor = MaterialTheme.colorScheme.surface) {
            ConversationModelChoices(snapshot, enabled, perform)
        }
    }
}

@Composable
private fun ConversationModelChoices(
    snapshot: Snapshot,
    enabled: Boolean,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
) {
    val key = snapshot.composerDraftKey()
    val draft = snapshot.draft(key)
    var providerOverride by remember(key) { mutableStateOf<String?>(null) }
    var search by remember(key) { mutableStateOf("") }
    var busy by remember(key) { mutableStateOf(false) }
    var error by remember(key) { mutableStateOf<String?>(null) }
    val newConversation = snapshot.navigation().threadId == null
    val provider = snapshot.providerSelection(key, if (newConversation) providerOverride else null)
    val canChoose = enabled && !busy
    fun execute(intent: Intent) {
        busy = true
        error = null
        perform(intent) { result ->
            busy = false
            error = result.exceptionOrNull()?.message
        }
    }
    LaunchedEffect(key) { execute(Intent.LoadModels(LoadModels())) }
    Column(
        Modifier.verticalScroll(rememberScrollState()).padding(horizontal = 24.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text("モデル", style = MaterialTheme.typography.titleLarge)
        ModelChoiceMenu(
            "Provider: ${provider?.let(snapshot::instanceName) ?: "未選択"}",
            snapshot.providerInstances().map { it.displayName to it.reference.instanceId },
            provider,
            canChoose && newConversation,
            Modifier.testTag("model.provider"),
        ) { id ->
            providerOverride = id
            id?.let { snapshot.modelForInstance(key, it) }?.let { execute(Intent.SelectModel(key, it)) }
        }
        ModelSearchField(search) { search = it }
        ConversationModelList(snapshot.modelsMatching(provider, search), draft.model, canChoose) {
            execute(Intent.SelectModel(key, it))
        }
        if (draft.model?.instanceId == provider) {
            ModelOptionChoices(snapshot.modelOptionControls(key), canChoose) { id, value ->
                execute(Intent.SelectModelOption(key, id, value))
            }
        }
        provider?.let { id ->
            val accounts = snapshot.accounts()?.accounts.orEmpty().filter { it.instanceId == id }
            val selected = accounts.firstOrNull { snapshot.accountIsSelected(id, it.id) }
            ConversationAccountChoices(
                accounts,
                selected,
                selected?.let { snapshot.accountWeeklyUsage(id, it.id) }.orEmpty(),
                canChoose,
            ) { account ->
                execute(Intent.SelectAccountForDraft(SelectAccountForDraft(id, account, key)))
            }
        }
        ModelCatalogStatus(
            busy,
            listOfNotNull(error, snapshot.accounts()?.error) + snapshot.modelErrorMessages(provider),
            canChoose,
        ) {
            execute(Intent.LoadModels(LoadModels()))
        }
    }
}

@Composable
private fun ModelSearchField(value: String, change: (String) -> Unit) {
    OutlinedTextField(
        value = value,
        onValueChange = change,
        label = { Text("モデルを検索") },
        singleLine = true,
        modifier = Modifier.testTag("model.search"),
    )
}

@Composable
private fun ConversationModelList(
    models: List<Model>,
    selected: ModelRef?,
    enabled: Boolean,
    choose: (ModelRef) -> Unit,
) {
    for (model in models) {
        TextButton(
            onClick = { choose(model.model) },
            enabled = enabled,
            modifier = Modifier.testTag("model.choice.${model.model.id}"),
        ) {
            Text("${if (model.model == selected) "✓ " else ""}${model.displayName}")
        }
    }
}

@Composable
private fun ConversationAccountChoices(
    accounts: List<Account>,
    selected: Account?,
    windows: List<UsageWindow>,
    enabled: Boolean,
    choose: (String) -> Unit,
) {
    if (accounts.isNotEmpty()) {
        ModelChoiceMenu(
            "アカウント: ${selected?.email ?: selected?.id ?: "未選択"}",
            accounts.map { (it.email ?: it.id) to it.id },
            selected?.id,
            enabled,
            Modifier.testTag("model.account"),
        ) { account ->
            account?.let(choose)
        }
    }
    for (window in windows) Text("${window.label}: ${window.remainingPercent}%")
}

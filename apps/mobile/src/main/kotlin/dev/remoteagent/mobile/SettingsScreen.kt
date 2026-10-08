// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "CyclomaticComplexMethod", "LongParameterList", "MagicNumber")

package dev.remoteagent.mobile

import android.net.Uri
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Archive
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Computer
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.ArchivedLayout
import dev.remoteagent.core.ArchivedOptions
import dev.remoteagent.core.ArchivedSortOrder
import dev.remoteagent.core.Intent
import dev.remoteagent.core.NativeUpdatePlatform
import dev.remoteagent.core.NativeUpdateRequest
import dev.remoteagent.core.ProviderKind
import dev.remoteagent.core.SettingControl
import dev.remoteagent.core.SettingValue
import dev.remoteagent.core.SettingsRow
import dev.remoteagent.core.SettingsScope
import dev.remoteagent.core.ThreadMenuConfirmation
import dev.remoteagent.core.UpdateChannel
import dev.remoteagent.core.accountErrorMessage
import dev.remoteagent.core.privacyPolicy

private const val PERCENT = 100f

@Composable
internal fun SettingsScreen(model: AndroidAppModel, projectId: String?) {
    val scope = projectId?.let { SettingsScope.Project(it) } ?: SettingsScope.Host
    LaunchedEffect(Unit) {
        model.perform(Intent.LoadConversationSettings)
        if (projectId == null) {
            model.perform(Intent.LoadAccounts)
            model.perform(Intent.LoadWorktreeSettings)
            model.perform(
                Intent.LoadNativeUpdate(
                    NativeUpdateRequest(
                        NativeUpdatePlatform.Android,
                        BuildConfig.VERSION_NAME,
                        UpdateChannel.Stable,
                    )
                )
            )
        }
    }
    val view = model.snapshot.settings(scope)
    ScreenScaffold(view.project?.label ?: "Settings", onBack = model::back) {
        LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) {
            if (projectId == null)
                item {
                    SectionCard("Connections") {
                        NavigationRow(Icons.Outlined.Computer, "Environments") { model.showHosts() }
                    }
                }
            if (projectId == null)
                item {
                    SectionCard("Projects & threads") {
                        NavigationRow(Icons.Outlined.Archive, "Archived Threads") { model.navigate(Route.Archived) }
                    }
                }
            view.project?.let { header ->
                item {
                    Row(
                        Modifier.padding(horizontal = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(16.dp),
                    ) {
                        ProjectFavicon(header.projectId, 48.dp)
                        Text(
                            header.label,
                            style = AppTheme.title,
                            fontWeight = FontWeight.Bold,
                            color = AppTheme.colors.foreground,
                        )
                    }
                }
            }
            view.project
                ?.takeIf { it.hasOverrides }
                ?.let { header ->
                    item {
                        TextButton(onClick = { model.perform(Intent.ResetProjectSettings(header.projectId)) }) {
                            Text("Reset project overrides", color = AppTheme.colors.dangerForeground)
                        }
                    }
                }
            items(view.sections, key = { it.id }) { section ->
                Column {
                    SectionCard(section.title) {
                        section.rows.forEachIndexed { index, row ->
                            if (index > 0) HorizontalDivider(color = AppTheme.colors.border)
                            SettingRow(
                                row,
                                onReset = { model.snapshot.settingReset(scope, row)?.let(model::perform) },
                            ) { value ->
                                model.snapshot.settingIntent(scope, row.id, value)?.let(model::perform)
                            }
                        }
                    }
                    section.footer?.let {
                        Text(
                            it,
                            Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                            style = AppTheme.caption,
                            color = AppTheme.colors.foregroundMuted,
                        )
                    }
                }
            }
            if (projectId == null) item { AccountsSection(model) }
            if (projectId == null) item { NativeUpdateSection(model) }
            if (projectId == null)
                item { Text(privacyPolicy(), style = AppTheme.caption, color = AppTheme.colors.foregroundMuted) }
        }
    }
}

@Composable
private fun NativeUpdateSection(model: AndroidAppModel) {
    val context = LocalContext.current
    val update = model.snapshot.nativeUpdate()
    SectionCard("App updates") {
        Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(
                when {
                    update == null -> "Checking for updates…"
                    update.updateAvailable -> "Version ${update.latestVersion ?: "new"} is available"
                    update.message != null -> update.message!!
                    else -> "Up to date"
                },
                style = AppTheme.body,
                color = AppTheme.colors.foreground,
            )
            update?.takeIf { it.updateAvailable }?.storeUrl?.let { url ->
                TextButton(onClick = {
                    val uri = Uri.parse(url)
                    if (uri.scheme == "https") {
                        context.startActivity(android.content.Intent(android.content.Intent.ACTION_VIEW, uri))
                    }
                }) {
                    Text("Open Play Store", color = AppTheme.colors.primaryText)
                }
            }
        }
    }
}

@Composable
private fun NavigationRow(icon: ImageVector, label: String, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onClick).padding(16.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Icon(icon, null, Modifier.size(24.dp), tint = AppTheme.colors.icon)
        Text(label, Modifier.weight(1f), style = AppTheme.headline, color = AppTheme.colors.foreground)
        Icon(Icons.Outlined.ChevronRight, null, Modifier.size(16.dp), tint = AppTheme.colors.chevron)
    }
}

@Composable
private fun SettingRow(row: SettingsRow, onReset: () -> Unit, onEdit: (SettingValue) -> Unit) {
    val colors = AppTheme.colors
    var open by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Column(
                Modifier.weight(1f)
                    .then(if (row.control is SettingControl.Choice) Modifier.clickable { open = !open } else Modifier)
            ) {
                Text(row.title, style = AppTheme.body, fontWeight = FontWeight.Medium, color = colors.foreground)
                row.description?.let { Text(it, style = AppTheme.caption, color = colors.foregroundMuted) }
            }
            when (val control = row.control) {
                is SettingControl.Switch ->
                    Switch(
                        control.on,
                        { onEdit(SettingValue.Switch(it)) },
                        colors =
                            SwitchDefaults.colors(
                                checkedTrackColor = colors.primary,
                                checkedThumbColor = colors.primaryForeground,
                                uncheckedTrackColor = colors.secondary,
                                uncheckedThumbColor = colors.iconMuted,
                            ),
                    )
                is SettingControl.Choice ->
                    Text(
                        control.choices.firstOrNull { it.id == control.selected }?.label.orEmpty(),
                        Modifier.clickable { open = !open },
                        style = AppTheme.footnote,
                        color = colors.foregroundSecondary,
                    )
                is SettingControl.Number ->
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        TextButton(
                            onClick = { onEdit(SettingValue.Number(control.value - 1u)) },
                            enabled = control.value > control.min,
                        ) {
                            Text("−", color = colors.foreground)
                        }
                        Text(control.value.toString(), style = AppTheme.body, color = colors.foreground)
                        TextButton(
                            onClick = { onEdit(SettingValue.Number(control.value + 1u)) },
                            enabled = control.value < control.max,
                        ) {
                            Text("+", color = colors.foreground)
                        }
                    }
                is SettingControl.Model ->
                    Text(
                        listOfNotNull(control.modelLabel, control.traitsLabel).joinToString(" · "),
                        style = AppTheme.footnote,
                        color = colors.foregroundSecondary,
                    )
            }
        }
        val choice = row.control as? SettingControl.Choice
        if (open && choice != null)
            choice.choices.forEach { option ->
                ChoiceRow(option.label, option.description, option.id == choice.selected) {
                    open = false
                    onEdit(SettingValue.Choice(option.id))
                }
            }
        if (row.resettable)
            Text("Reset", Modifier.clickable(onClick = onReset), style = AppTheme.caption, color = colors.primaryText)
    }
}

@Composable
private fun AccountsSection(model: AndroidAppModel) {
    val context = LocalContext.current
    val colors = AppTheme.colors
    var code by remember { mutableStateOf("") }
    SectionCard("Provider accounts") {
        model.snapshot.accounts()?.accounts.orEmpty().forEach { account ->
            Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        account.email ?: account.id,
                        Modifier.weight(1f),
                        style = AppTheme.body,
                        color = colors.foreground,
                    )
                    TextButton(onClick = { model.perform(Intent.SelectAccount(account.provider, account.id)) }) {
                        Text("Select", color = colors.primaryText)
                    }
                    TextButton(onClick = { model.perform(Intent.DeleteAccount(account.provider, account.id)) }) {
                        Text("Remove", color = colors.dangerForeground)
                    }
                }
                account.usage?.windows?.forEach { window ->
                    Text(
                        "${window.label}: ${window.remainingPercent}% remaining",
                        style = AppTheme.caption,
                        color = colors.foregroundMuted,
                    )
                    LinearProgressIndicator(
                        progress = { window.remainingPercent.toFloat() / PERCENT },
                        modifier = Modifier.fillMaxWidth(),
                        color = colors.primary,
                        trackColor = colors.secondary,
                    )
                }
                account.usage?.error?.let {
                    Text(accountErrorMessage(it), style = AppTheme.caption, color = colors.warningForeground)
                }
            }
            HorizontalDivider(color = colors.border)
        }
        Row(Modifier.padding(horizontal = 8.dp)) {
            TextButton(onClick = { model.perform(Intent.StartLogin(ProviderKind.CODEX)) }) {
                Text("Sign in to Codex", color = colors.foreground)
            }
            TextButton(onClick = { model.perform(Intent.StartLogin(ProviderKind.CLAUDE)) }) {
                Text("Sign in to Claude", color = colors.foreground)
            }
        }
        model.snapshot.accountLogin()?.let { login ->
            Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                if (login.userCode.isNotBlank())
                    Text(login.userCode, style = AppTheme.title, fontFamily = AppTheme.mono)
                TextButton(
                    onClick = {
                        val uri = Uri.parse(login.verificationUrl)
                        if (uri.scheme == "https")
                            context.startActivity(android.content.Intent(android.content.Intent.ACTION_VIEW, uri))
                    }
                ) {
                    Text("Open sign-in page", color = colors.primaryText)
                }
                if (login.requiresCodeSubmission) {
                    SettingsField(code, { code = it }, "Authorization code")
                    PrimaryButton("Complete sign in", enabled = code.isNotBlank()) {
                        model.perform(Intent.CompleteLogin(login.provider, login.loginId, code))
                        code = ""
                    }
                }
                TextButton(
                    onClick = {
                        model.perform(Intent.CancelLogin(login.provider, login.loginId))
                        code = ""
                    }
                ) {
                    Text("Cancel sign in", color = colors.foreground)
                }
            }
        }
    }
}

@Composable
internal fun ArchivedScreen(model: AndroidAppModel) {
    val now = rememberNow()
    var search by remember { mutableStateOf("") }
    var confirming by remember { mutableStateOf<Pair<ThreadMenuConfirmation, () -> Unit>?>(null) }
    val options = ArchivedOptions(ArchivedLayout.SCREEN, search, ArchivedSortOrder.NEWEST, confirmDelete = true)
    val view by rememberView(model.snapshot, now / MINUTE, options) { it.archived(System.currentTimeMillis(), options) }
    val colors = AppTheme.colors
    ScreenScaffold("Archived Threads", onBack = model::back) {
        Column(Modifier.fillMaxSize()) {
            SettingsField(search, { search = it }, "Search", Modifier.padding(16.dp))
            val current = view
            current?.empty?.let { empty ->
                Column(Modifier.fillMaxWidth().padding(28.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(empty.title, style = AppTheme.title, fontWeight = FontWeight.Bold, color = colors.foreground)
                    empty.detail?.let { Text(it, style = AppTheme.body, color = colors.foregroundMuted) }
                }
            }
            current?.error?.let { Text(it, Modifier.padding(16.dp), color = colors.dangerForeground) }
            LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) {
                items(current?.groups.orEmpty(), key = { it.projectId }) { group ->
                    Row(
                        Modifier.padding(start = 4.dp, end = 4.dp, bottom = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        ProjectFavicon(group.projectId, 18.dp)
                        Text(
                            group.title.uppercase(),
                            style = AppTheme.label,
                            fontWeight = FontWeight.Medium,
                            letterSpacing = 0.5.sp,
                            color = colors.foregroundMuted,
                            maxLines = 1,
                        )
                    }
                    SectionCard(null) {
                        group.rows.forEachIndexed { index, row ->
                            if (index > 0) HorizontalDivider(color = colors.border)
                            Column(Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(16.dp)) {
                                Text(
                                    row.title,
                                    style = AppTheme.body,
                                    fontWeight = FontWeight.Medium,
                                    color = colors.foreground,
                                )
                                Text(row.description, style = AppTheme.caption, color = colors.foregroundMuted)
                                Row {
                                    row.actions.forEach { action ->
                                        TextButton(
                                            onClick = {
                                                val run = { model.perform(Intent.Thread(row.threadId, action.action)) }
                                                val confirmation = action.confirmation
                                                if (confirmation != null) confirming = confirmation to { run() }
                                                else run()
                                            }
                                        ) {
                                            Text(
                                                action.label,
                                                color =
                                                    if (action.destructive) colors.dangerForeground
                                                    else colors.primaryText,
                                            )
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    confirming?.let { (confirmation, run) ->
        ConfirmDialog(
            confirmation,
            "Delete",
            onConfirm = {
                confirming = null
                run()
            },
        ) {
            confirming = null
        }
    }
}

private const val MINUTE = 60_000L

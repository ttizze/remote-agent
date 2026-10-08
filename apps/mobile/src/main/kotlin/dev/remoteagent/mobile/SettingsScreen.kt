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
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
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
import dev.remoteagent.core.DiagnosticRow
import dev.remoteagent.core.Intent
import dev.remoteagent.core.NativeUpdatePlatform
import dev.remoteagent.core.NativeUpdateRequest
import dev.remoteagent.core.ProviderKind
import dev.remoteagent.core.SettingControl
import dev.remoteagent.core.SettingSource
import dev.remoteagent.core.SettingValue
import dev.remoteagent.core.SettingsRow
import dev.remoteagent.core.SettingsScope
import dev.remoteagent.core.ThreadMenuConfirmation
import dev.remoteagent.core.UpdateChannel
import dev.remoteagent.core.accountErrorMessage
import dev.remoteagent.core.privacyPolicy
import java.util.UUID

private const val PERCENT = 100f

private fun releaseUpdateChannel(): UpdateChannel =
    when (BuildConfig.RELEASE_CHANNEL) {
        "nightly" -> UpdateChannel.Nightly
        "preview" -> UpdateChannel.Preview
        else -> UpdateChannel.Stable
    }

@Composable
internal fun SettingsScreen(model: AndroidAppModel, projectId: String?) {
    val scope = projectId?.let { SettingsScope.Project(it) } ?: SettingsScope.Host
    LaunchedEffect(Unit) {
        model.perform(Intent.LoadSettings)
        if (projectId == null) {
            model.perform(Intent.LoadAccounts)
            model.perform(Intent.LoadProviders)
            model.perform(Intent.LoadWorktreeSettings)
            model.perform(
                Intent.LoadNativeUpdate(
                    NativeUpdateRequest(NativeUpdatePlatform.Android, BuildConfig.VERSION_NAME, releaseUpdateChannel())
                )
            )
            model.perform(Intent.LoadBackgroundPolicy)
        }
    }
    val view = model.snapshot.settings(scope)
    ScreenScaffold(view.project?.label ?: "Settings", onBack = model::back) {
        LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) {
            if (projectId == null)
                item {
                    SectionCard("Connections") {
                        NavigationRow(Icons.Outlined.Computer, "Environments (${model.environmentSettings().size})") {
                            model.showHosts()
                        }
                    }
                }
            if (projectId == null)
                item {
                    SectionCard("Interface") {
                        NavigationRow(Icons.Outlined.Computer, "Appearance") { model.navigate(Route.Appearance) }
                    }
                }
            if (projectId == null)
                item {
                    SectionCard("Projects & threads") {
                        NavigationRow(Icons.Outlined.Archive, "Archived Threads") { model.navigate(Route.Archived) }
                        NavigationRow(Icons.Outlined.Schedule, "Scheduled tasks") {
                            model.navigate(Route.ScheduledTasks)
                        }
                    }
                }
            if (projectId == null)
                item {
                    SectionCard("Server settings") {
                        NavigationRow(Icons.Outlined.Computer, "Usage") { model.navigate(Route.Usage) }
                    }
                }
            if (projectId == null) item { LoadBalancingSettings(model) }
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
                val rows = section.rows.filterNot { projectId == null && it.id == SettingId.LoadBalancing }
                Column {
                    SectionCard(section.title) {
                        rows.forEachIndexed { index, row ->
                            if (index > 0) HorizontalDivider(color = AppTheme.colors.border)
                            SettingRow(
                                row,
                                onReset = { model.snapshot.settingReset(scope, row)?.let(model::perform) },
                                onIntent = model::perform,
                                onRemoveBrowserProfile = model::removeBrowserProfile,
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
            if (projectId == null) item { BackgroundDiagnosticsSection(model) }
            if (projectId == null)
                item { Text(privacyPolicy(), style = AppTheme.caption, color = AppTheme.colors.foregroundMuted) }
        }
    }
}

@Composable
private fun LoadBalancingSettings(model: AndroidAppModel) {
    val rows = model.loadBalancingPreferences()
    SectionCard("Load balancing") {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text("Automatic routing", style = AppTheme.body)
                Text(
                    "Choose a connected environment for matching new threads.",
                    style = AppTheme.caption,
                    color = AppTheme.colors.foregroundMuted,
                )
            }
            Switch(
                checked = model.snapshot.preferences().loadBalancingEnabled,
                onCheckedChange = model::setLoadBalancingEnabled,
                colors = SwitchDefaults.colors(checkedThumbColor = AppTheme.colors.primaryText),
            )
        }
        if (rows.size < 2) {
            Text(
                "Connect at least two environments to choose weights.",
                Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                style = AppTheme.caption,
                color = AppTheme.colors.foregroundMuted,
            )
        } else {
            rows.forEach { row ->
                HorizontalDivider(color = AppTheme.colors.border)
                Column(Modifier.fillMaxWidth().padding(16.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(row.environmentLabel, style = AppTheme.body, modifier = Modifier.weight(1f))
                        Text(row.connectionState, style = AppTheme.caption, color = AppTheme.colors.foregroundMuted)
                    }
                    Row(
                        Modifier.fillMaxWidth().padding(top = 6.dp),
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        listOf(100u to "Prefer", 50u to "Normal", 25u to "Less often", 0u to "Manual only").forEach {
                            (weight, label) ->
                            TextButton(
                                enabled = model.snapshot.preferences().loadBalancingEnabled,
                                onClick = { model.setLoadBalancingWeight(row.environmentId, weight.toUByte()) },
                            ) {
                                Text(
                                    label,
                                    color =
                                        if (row.weight.toUInt() == weight) AppTheme.colors.primaryText
                                        else AppTheme.colors.foregroundMuted,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
internal fun AppearanceScreen(model: AndroidAppModel) {
    val context = LocalContext.current
    var appearance by remember { mutableStateOf(AppTheme.appearance) }
    var themeTarget by remember { mutableStateOf("both") }
    fun update(next: MobileAppearanceSettings) {
        appearance = next.normalized()
        AppTheme.update(context, appearance)
        model.perform(Intent.SetTerminalFontSize(appearance.resolvedTerminalFontSize()))
    }
    ScreenScaffold("Appearance", onBack = model::back) {
        LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) {
            item {
                SectionCard("Color scheme") {
                    MobileColorScheme.entries.forEach { scheme ->
                        Row(
                            Modifier.fillMaxWidth()
                                .clickable { update(appearance.copy(colorScheme = scheme)) }
                                .padding(16.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(
                                appearance.colorScheme == scheme,
                                { update(appearance.copy(colorScheme = scheme)) },
                            )
                            Text(scheme.name.lowercase().replaceFirstChar { it.uppercase() }, style = AppTheme.body)
                        }
                    }
                }
            }
            item {
                SectionCard("Themes") {
                    Row(
                        Modifier.fillMaxWidth().padding(horizontal = 16.dp),
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        listOf("both" to "Both", "light" to "Light", "dark" to "Dark").forEach { (id, label) ->
                            TextButton(onClick = { themeTarget = id }) {
                                Text(
                                    label,
                                    color =
                                        if (themeTarget == id) AppTheme.colors.primaryText
                                        else AppTheme.colors.foregroundMuted,
                                )
                            }
                        }
                    }
                    listOf(
                            null to "Bex",
                            "chat" to "Chat",
                            "grove" to "Grove",
                            "ocean" to "Ocean",
                            "ember" to "Ember",
                            "iris" to "Iris",
                            "material-you" to "Material You",
                        )
                        .forEach { (id, label) ->
                            val selected =
                                when (themeTarget) {
                                    "light" -> appearance.lightTheme == id
                                    "dark" -> appearance.darkTheme == id
                                    else ->
                                        appearance.theme == id &&
                                            appearance.lightTheme == null &&
                                            appearance.darkTheme == null
                                }
                            val pick = {
                                update(
                                    when (themeTarget) {
                                        "light" -> appearance.assigningTheme(false, id)
                                        "dark" -> appearance.assigningTheme(true, id)
                                        else -> appearance.copy(theme = id, lightTheme = null, darkTheme = null)
                                    }
                                )
                            }
                            Row(
                                Modifier.fillMaxWidth()
                                    .clickable(onClick = pick)
                                    .padding(horizontal = 16.dp, vertical = 10.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                RadioButton(selected, pick)
                                Text(label, style = AppTheme.body)
                            }
                        }
                }
            }
            item {
                SectionCard("Text") {
                    SizeRow("Base size", appearance.baseFontSize, 11, 22) { update(appearance.copy(baseFontSize = it)) }
                }
            }
            item {
                SectionCard("Code") {
                    ToggleRow("Custom size", appearance.codeFontSize != null) {
                        update(appearance.copy(codeFontSize = if (it) appearance.codeFontSize ?: 12 else null))
                    }
                    appearance.codeFontSize?.let { size ->
                        SizeRow("Code size", size, 8, 18) { update(appearance.copy(codeFontSize = it)) }
                    }
                    ToggleRow("Wrap long lines", appearance.codeWordWrap) { update(appearance.copy(codeWordWrap = it)) }
                }
            }
            item {
                SectionCard("Terminal") {
                    ToggleRow("Custom size", appearance.terminalFontSize != null) {
                        update(
                            appearance.copy(terminalFontSize = if (it) appearance.terminalFontSize ?: 10.5 else null)
                        )
                    }
                    appearance.terminalFontSize?.let { size ->
                        SizeRow("Terminal size", size, 6.0, 14.0, 0.5) {
                            update(appearance.copy(terminalFontSize = it))
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun BackgroundDiagnosticsSection(model: AndroidAppModel) {
    val colors = AppTheme.colors
    val rows = model.snapshot.backgroundRows()
    SectionCard("Background activity & diagnostics") {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("Host policy", Modifier.weight(1f), style = AppTheme.body, color = colors.foreground)
            TextButton(onClick = { model.perform(Intent.LoadDiagnostics("")) }) {
                Text("Refresh", color = colors.primaryText)
            }
        }
        rows.forEachIndexed { index, row ->
            if (
                row.key == "profile" ||
                    row.key == "automaticGitFetchIntervalMs" ||
                    row.key == "providerHealthRefreshIntervalMs"
            )
                return@forEachIndexed
            if (index > 0) HorizontalDivider(color = colors.border)
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(row.key, Modifier.weight(1f), style = AppTheme.caption, color = colors.foregroundMuted)
                Text(row.value, style = AppTheme.caption, color = colors.foregroundSecondary)
            }
        }
        val profile = rows.firstOrNull { it.key == "profile" }?.value?.lowercase()
        if (profile != null) {
            HorizontalDivider(color = colors.border)
            listOf("balanced", "performance", "battery-saver").forEach { choice ->
                ChoiceRow(choice.replace('-', ' ').replaceFirstChar { it.uppercase() }, null, profile == choice) {
                    model.perform(Intent.SetBackgroundProfile(choice))
                }
            }
        }
        BackgroundIntervalPicker(
            "Git fetch interval",
            backgroundIntervalSeconds(rows, "automaticGitFetchIntervalMs"),
            listOf(0, 15, 30, 60, 300, 900),
        ) { seconds ->
            model.perform(Intent.SetAutomaticGitFetchInterval(seconds))
        }
        BackgroundIntervalPicker(
            "Provider health interval",
            backgroundIntervalSeconds(rows, "providerHealthRefreshIntervalMs"),
            listOf(0, 60, 300, 900, 1800),
        ) { seconds ->
            model.perform(Intent.SetProviderHealthRefreshInterval(seconds))
        }
        model.snapshot.hostResourceRows().forEach { row ->
            Text(
                "${row.key}: ${row.value}",
                Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                style = AppTheme.caption,
                color = colors.foregroundMuted,
            )
        }
        model.snapshot.processRows().take(8).forEach { row ->
            Text(
                "${row.key}: ${row.value}",
                Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                style = AppTheme.caption,
                color = colors.foregroundMuted,
            )
        }
        model.snapshot.processHistoryRows().take(8).forEach { row ->
            Text(
                "${row.key}: ${row.value}",
                Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                style = AppTheme.caption,
                color = colors.foregroundMuted,
            )
        }
        model.snapshot.traceRows().forEach { row ->
            Text(
                "${row.key}: ${row.value}",
                Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                style = AppTheme.caption,
                color = colors.foregroundMuted,
            )
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
            update
                ?.takeIf { it.updateAvailable }
                ?.storeUrl
                ?.let { url ->
                    TextButton(
                        onClick = {
                            val uri = Uri.parse(url)
                            if (uri.scheme == "https") {
                                context.startActivity(android.content.Intent(android.content.Intent.ACTION_VIEW, uri))
                            }
                        }
                    ) {
                        Text("Open Play Store", color = AppTheme.colors.primaryText)
                    }
                }
        }
    }
}

@Composable
private fun BackgroundIntervalPicker(title: String, selectedSeconds: Int, values: List<Int>, onChange: (Int) -> Unit) {
    val options = (values + selectedSeconds).distinct().sorted()
    val index = options.indexOf(selectedSeconds)
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, Modifier.weight(1f), style = AppTheme.caption, color = AppTheme.colors.foregroundMuted)
        TextButton(onClick = { onChange(options[index - 1]) }, enabled = index > 0) {
            Text("−", color = AppTheme.colors.foreground)
        }
        Text(
            if (selectedSeconds == 0) "Disabled" else "$selectedSeconds s",
            style = AppTheme.caption,
            color = AppTheme.colors.foregroundSecondary,
        )
        TextButton(onClick = { onChange(options[index + 1]) }, enabled = index >= 0 && index + 1 < options.size) {
            Text("+", color = AppTheme.colors.foreground)
        }
    }
}

@Composable
private fun ToggleRow(label: String, checked: Boolean, onCheckedChange: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f), style = AppTheme.body)
        Switch(checked, onCheckedChange)
    }
}

@Composable
private fun SizeRow(label: String, value: Int, min: Int, max: Int, onChange: (Int) -> Unit) {
    Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f), style = AppTheme.body)
        TextButton(onClick = { if (value > min) onChange(value - 1) }, enabled = value > min) { Text("−") }
        Text(value.toString(), style = AppTheme.body.copy(fontFamily = AppTheme.mono))
        TextButton(onClick = { if (value < max) onChange(value + 1) }, enabled = value < max) { Text("+") }
    }
}

@Composable
private fun SizeRow(label: String, value: Double, min: Double, max: Double, step: Double, onChange: (Double) -> Unit) {
    Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f), style = AppTheme.body)
        TextButton(onClick = { if (value > min) onChange((value - step).coerceAtLeast(min)) }, enabled = value > min) {
            Text("−")
        }
        Text(String.format("%.1f", value), style = AppTheme.body.copy(fontFamily = AppTheme.mono))
        TextButton(onClick = { if (value < max) onChange((value + step).coerceAtMost(max)) }, enabled = value < max) {
            Text("+")
        }
    }
}

private fun backgroundIntervalSeconds(rows: List<DiagnosticRow>, key: String): Int =
    rows
        .firstOrNull { it.key == key }
        ?.value
        ?.toLongOrNull()
        ?.div(1_000L)
        ?.coerceAtMost(Int.MAX_VALUE.toLong())
        ?.toInt() ?: 0

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
private fun SettingRow(
    row: SettingsRow,
    onReset: () -> Unit,
    onIntent: (Intent) -> Unit,
    onRemoveBrowserProfile: (String) -> Unit,
    onEdit: (SettingValue) -> Unit,
) {
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
                is SettingControl.Text -> {
                    var text by remember(row.id, control.value) { mutableStateOf(control.value) }
                    OutlinedTextField(
                        value = text,
                        onValueChange = {
                            text = it
                            onEdit(SettingValue.Text(it))
                        },
                        modifier = Modifier.fillMaxWidth(0.58f),
                        placeholder = { control.placeholder?.let { Text(it) } },
                        singleLine = true,
                    )
                }
                is SettingControl.BrowserProfiles ->
                    BrowserProfilesControl(control, onEdit, onIntent, onRemoveBrowserProfile)
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
        row.source?.let { source ->
            Text(
                if (source == SettingSource.Project) "Project override" else "Inherited from Host",
                style = AppTheme.caption,
                color = colors.primaryText,
            )
        }
    }
}

@Composable
private fun BrowserProfilesControl(
    control: SettingControl.BrowserProfiles,
    onDefault: (SettingValue) -> Unit,
    onIntent: (Intent) -> Unit,
    onRemove: (String) -> Unit,
) {
    val colors = AppTheme.colors
    var editingId by remember { mutableStateOf<String?>(null) }
    var editingName by remember { mutableStateOf("") }
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        control.profiles.forEach { profile ->
            val builtIn = profile.id == "default" || profile.id == "incognito"
            Row(
                Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(profile.name, Modifier.weight(1f), style = AppTheme.body, color = colors.foreground)
                if (profile.id != "incognito") {
                    TextButton(onClick = { onDefault(SettingValue.Choice(profile.id)) }) {
                        Text(
                            if (profile.id == control.defaultProfileId) "Default" else "Use",
                            color = colors.primaryText,
                        )
                    }
                }
                if (!builtIn) {
                    TextButton(
                        onClick = {
                            editingId = profile.id
                            editingName = profile.name
                        }
                    ) {
                        Text("Rename", color = colors.primaryText)
                    }
                    TextButton(onClick = { onRemove(profile.id) }) { Text("Remove", color = colors.dangerForeground) }
                }
            }
            if (editingId == profile.id) {
                OutlinedTextField(
                    value = editingName,
                    onValueChange = { editingName = it },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                    label = { Text("Profile name") },
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(
                        onClick = {
                            onIntent(Intent.RenameBrowserProfile(profile.id, editingName))
                            editingId = null
                        }
                    ) {
                        Text("Save", color = colors.primaryText)
                    }
                    TextButton(onClick = { editingId = null }) { Text("Cancel", color = colors.foregroundMuted) }
                }
            }
        }
        TextButton(
            enabled = control.profiles.count { it.id != "default" && it.id != "incognito" } < 24,
            onClick = { onIntent(Intent.CreateBrowserProfile(UUID.randomUUID().toString(), null)) },
        ) {
            Text("New profile", color = colors.primaryText)
        }
    }
}

@Composable
private fun AccountsSection(model: AndroidAppModel) {
    val context = LocalContext.current
    val colors = AppTheme.colors
    var code by remember { mutableStateOf("") }
    SectionCard("Provider accounts") {
        model.snapshot.accounts()?.accounts.orEmpty().forEach { account ->
            val limits = model.snapshot.usageLimits().firstOrNull { account.id in it.sourceAccountIds }
            var confirmingReset by remember(account.id) { mutableStateOf(false) }
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
                limits?.windows?.forEach { window ->
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
                if ((limits?.resetCreditCount ?: 0) > 0) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            "Reset credits: ${limits?.resetCreditCount}",
                            style = AppTheme.caption,
                            color = colors.foregroundMuted,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(onClick = { confirmingReset = true }) {
                            Text("Use reset", color = colors.primaryText)
                        }
                    }
                }
                limits?.externalLabel?.let {
                    TextButton(
                        onClick = {
                            limits.externalUrl?.let { url ->
                                val uri = Uri.parse(url)
                                if (uri.scheme == "https") {
                                    context.startActivity(
                                        android.content.Intent(android.content.Intent.ACTION_VIEW, uri)
                                    )
                                }
                            }
                        }
                    ) {
                        Text(it, style = AppTheme.caption, color = colors.primaryText)
                    }
                }
                limits?.error?.let {
                    Text(accountErrorMessage(it), style = AppTheme.caption, color = colors.warningForeground)
                }
            }
            if (confirmingReset) {
                AlertDialog(
                    onDismissRequest = { confirmingReset = false },
                    title = { Text("Use a reset credit?") },
                    text = { Text("This redeems one credit and clears the current rate-limit windows.") },
                    confirmButton = {
                        TextButton(
                            onClick = {
                                confirmingReset = false
                                val sourceId = limits?.resetCreditAccountId ?: account.id
                                val source = model.snapshot.accounts()?.accounts?.firstOrNull { it.id == sourceId }
                                model.perform(
                                    Intent.ConsumeResetCredit(
                                        source?.provider ?: account.provider,
                                        sourceId,
                                        limits?.nextCreditId,
                                    )
                                )
                                model.perform(Intent.LoadAccounts)
                            }
                        ) {
                            Text("Use credit")
                        }
                    },
                    dismissButton = { TextButton(onClick = { confirmingReset = false }) { Text("Cancel") } },
                )
            }
            HorizontalDivider(color = colors.border)
        }
        model.snapshot.providerAdvisories().forEach { advisory ->
            Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(advisory.displayName, Modifier.weight(1f), style = AppTheme.body, color = colors.foreground)
                    Text(advisory.status, style = AppTheme.caption, color = colors.foregroundMuted)
                }
                if (advisory.currentVersion != null && advisory.latestVersion != null) {
                    Text(
                        "${advisory.currentVersion} → ${advisory.latestVersion}",
                        style = AppTheme.caption,
                        color = colors.foregroundMuted,
                    )
                }
                advisory.message?.let { Text(it, style = AppTheme.caption, color = colors.foregroundMuted) }
                if (advisory.canUpdate) {
                    TextButton(
                        onClick = {
                            model.perform(Intent.UpdateProvider(advisory.instanceId, null)) { result ->
                                if (result.isSuccess) model.perform(Intent.LoadProviders)
                            }
                        }
                    ) {
                        Text("Update provider", color = colors.primaryText)
                    }
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

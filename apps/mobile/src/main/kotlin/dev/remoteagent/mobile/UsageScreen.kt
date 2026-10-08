package dev.remoteagent.mobile

import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.ComposerUsageLimits
import dev.remoteagent.core.Intent
import dev.remoteagent.core.PriceOverride
import dev.remoteagent.core.Resolution
import dev.remoteagent.core.UsagePreferences
import dev.remoteagent.core.UsageSummaryInput

@Composable
internal fun UsageLimitsRow(limits: ComposerUsageLimits, onClick: () -> Unit) {
    TextButton(onClick = onClick) {
        val remaining = limits.windows.firstOrNull()?.remainingPercent
        Text(
            if (remaining == null) "Usage limits" else "Limits $remaining% left",
            color = AppTheme.colors.foregroundMuted,
        )
    }
}

@Composable
internal fun UsageScreen(model: AndroidAppModel, initialTab: UsageTab = UsageTab.USAGE) {
    val view = model.snapshot.usagePage()
    var tab by remember { mutableStateOf(initialTab) }
    LaunchedEffect(model.usageDeepLinkRequests) {
        if (model.usageDeepLinkRequests > 0) {
            tab = UsageTab.LIMITS
            model.consumeUsageDeepLinkRequest()
        }
        loadUsage(model)
    }
    ScreenScaffold(
        "Usage",
        onBack = model::back,
        actions = {
            TextButton(onClick = { loadUsage(model) }) { Text("Refresh") }
            TextButton(
                onClick = {
                    model.perform(Intent.RefreshUsageRates) { result -> if (result.isSuccess) loadUsage(model) }
                }
            ) {
                Text("Refresh rates")
            }
        },
    ) {
        LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            item {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Center) {
                    TextButton(onClick = { tab = UsageTab.USAGE }) {
                        Text(
                            "Usage",
                            color =
                                if (tab == UsageTab.USAGE) AppTheme.colors.primaryText
                                else AppTheme.colors.foregroundMuted,
                        )
                    }
                    TextButton(onClick = { tab = UsageTab.LIMITS }) {
                        Text(
                            "Limits",
                            color =
                                if (tab == UsageTab.LIMITS) AppTheme.colors.primaryText
                                else AppTheme.colors.foregroundMuted,
                        )
                    }
                }
            }
            if (tab == UsageTab.USAGE) {
                item {
                    SectionCard("Summary") {
                        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                            Text("Tokens: ${view.totalTokensLabel} · ${view.sessions} sessions", style = AppTheme.body)
                            Text("Estimated cost: ${view.costLabel} · ${view.pricingStatus}", style = AppTheme.body)
                            if (view.state == "loading") Text("Loading…", style = AppTheme.caption)
                            view.error?.let {
                                Text(it, style = AppTheme.caption, color = AppTheme.colors.dangerForeground)
                            }
                        }
                    }
                }
                item { Text("Usage by model", style = AppTheme.title) }
                items(view.rows, key = { "${it.day}:${it.provider}:${it.model}" }) { row ->
                    Column(
                        Modifier.fillMaxWidth().padding(horizontal = 8.dp),
                        verticalArrangement = Arrangement.spacedBy(3.dp),
                    ) {
                        Text("${row.provider} · ${row.model}", style = AppTheme.body)
                        Text(
                            "${row.day} · ${row.tokens} tokens · ${row.costLabel}",
                            style = AppTheme.caption,
                            color = AppTheme.colors.foregroundMuted,
                        )
                    }
                }
                if (view.rows.isEmpty() && view.state == "ready")
                    item {
                        Text(
                            "No usage in this period.",
                            style = AppTheme.caption,
                            color = AppTheme.colors.foregroundMuted,
                        )
                    }
                item { Text("Daily trend", style = AppTheme.title) }
                items(view.chart, key = { it.day }) { point ->
                    Column(
                        Modifier.fillMaxWidth().padding(horizontal = 8.dp),
                        verticalArrangement = Arrangement.spacedBy(3.dp),
                    ) {
                        Text(point.day, style = AppTheme.body)
                        Text(
                            "${point.tokens} tokens · ${"%.2f".format(point.costUsd)} USD",
                            style = AppTheme.caption,
                            color = AppTheme.colors.foregroundMuted,
                        )
                    }
                }
                item { UsagePreferencesEditor(model, model.snapshot.usagePreferences()) }
            } else {
                item {
                    SectionCard("Limits") {
                        val pools = model.snapshot.usageLimitPools(System.currentTimeMillis())
                        val accounts = model.snapshot.usageLimits()
                        val sourceAccounts = model.snapshot.accounts()?.accounts.orEmpty()
                        val pooledIds = pools.flatMap { it.accounts }.map { it.id }.toSet()
                        fun consumeReset(account: dev.remoteagent.core.UsageLimitAccount) {
                            val sourceId = account.resetCreditAccountId ?: account.id
                            sourceAccounts
                                .firstOrNull { it.id == sourceId }
                                ?.let { source ->
                                    model.perform(
                                        Intent.ConsumeResetCredit(source.provider, source.id, account.nextCreditId)
                                    )
                                    model.perform(Intent.LoadAccounts)
                                }
                        }
                        if (pools.isEmpty() && accounts.isEmpty()) {
                            Text(
                                "Provider limits are unavailable until accounts are loaded.",
                                Modifier.padding(16.dp),
                                style = AppTheme.caption,
                            )
                        } else {
                            pools.forEachIndexed { index, pool ->
                                if (index > 0) HorizontalDivider(color = AppTheme.colors.border)
                                UsageLimitPoolView(pool) { account -> consumeReset(account) }
                            }
                            accounts
                                .filter { it.id !in pooledIds }
                                .forEach { account ->
                                    HorizontalDivider(color = AppTheme.colors.border)
                                    UsageLimitAccount(account, showWindows = false) { consumeReset(account) }
                                }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun UsagePreferencesEditor(model: AndroidAppModel, preferences: UsagePreferences) {
    val colors = AppTheme.colors
    var aliasModel by remember { mutableStateOf("") }
    var aliasTarget by remember { mutableStateOf("") }
    var priceModel by remember { mutableStateOf("") }
    var inputPrice by remember { mutableStateOf("") }
    var outputPrice by remember { mutableStateOf("") }
    var cacheReadPrice by remember { mutableStateOf("") }
    var cacheWritePrice by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }

    fun save(next: UsagePreferences) {
        error = null
        model.perform(Intent.SetUsagePreferences(next))
        loadUsage(model)
    }

    SectionCard("Usage preferences") {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Model aliases", style = AppTheme.title)
            preferences.modelAliases.toSortedMap().forEach { (modelName, alias) ->
                Row(Modifier.fillMaxWidth(), verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                    Text("$modelName → $alias", style = AppTheme.caption, modifier = Modifier.weight(1f))
                    TextButton(
                        onClick = {
                            val aliases = preferences.modelAliases.toMutableMap()
                            aliases.remove(modelName)
                            save(UsagePreferences(modelAliases = aliases, priceOverrides = preferences.priceOverrides))
                        }
                    ) {
                        Text("Remove", color = colors.primaryText)
                    }
                }
            }
            SettingsField(aliasModel, { aliasModel = it }, "Source model")
            SettingsField(aliasTarget, { aliasTarget = it }, "Bucket and pricing model")
            TextButton(
                enabled = aliasModel.isNotBlank() && aliasTarget.isNotBlank(),
                onClick = {
                    val aliases = preferences.modelAliases.toMutableMap()
                    aliases[aliasModel.trim()] = aliasTarget.trim()
                    save(UsagePreferences(modelAliases = aliases, priceOverrides = preferences.priceOverrides))
                    aliasModel = ""
                    aliasTarget = ""
                },
            ) {
                Text("Save alias", color = colors.primaryText)
            }

            Text("Price overrides (USD per million tokens)", style = AppTheme.title)
            preferences.priceOverrides.toSortedMap().forEach { (modelName, prices) ->
                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(modelName, style = AppTheme.body)
                    Text(
                        "input ${prices.inputCostPerMillionTokens} · output ${prices.outputCostPerMillionTokens}",
                        style = AppTheme.caption,
                        color = colors.foregroundMuted,
                    )
                    TextButton(
                        onClick = {
                            val overrides = preferences.priceOverrides.toMutableMap()
                            overrides.remove(modelName)
                            save(UsagePreferences(modelAliases = preferences.modelAliases, priceOverrides = overrides))
                        }
                    ) {
                        Text("Remove", color = colors.primaryText)
                    }
                }
            }
            SettingsField(priceModel, { priceModel = it }, "Model price key")
            SettingsField(inputPrice, { inputPrice = it }, "Input price")
            SettingsField(outputPrice, { outputPrice = it }, "Output price")
            SettingsField(cacheReadPrice, { cacheReadPrice = it }, "Cache read price (optional)")
            SettingsField(cacheWritePrice, { cacheWritePrice = it }, "Cache write price (optional)")
            TextButton(
                enabled =
                    priceModel.isNotBlank() &&
                        inputPrice.toDoubleOrNull() != null &&
                        outputPrice.toDoubleOrNull() != null,
                onClick = {
                    val input = inputPrice.toDoubleOrNull()
                    val output = outputPrice.toDoubleOrNull()
                    if (input == null || output == null) {
                        error = "Enter numeric input and output prices."
                    } else {
                        val overrides = preferences.priceOverrides.toMutableMap()
                        overrides[priceModel.trim()] =
                            PriceOverride(
                                inputCostPerMillionTokens = input,
                                outputCostPerMillionTokens = output,
                                cacheReadCostPerMillionTokens = cacheReadPrice.toDoubleOrNull(),
                                cacheWriteCostPerMillionTokens = cacheWritePrice.toDoubleOrNull(),
                            )
                        save(UsagePreferences(modelAliases = preferences.modelAliases, priceOverrides = overrides))
                        priceModel = ""
                        inputPrice = ""
                        outputPrice = ""
                        cacheReadPrice = ""
                        cacheWritePrice = ""
                    }
                },
            ) {
                Text("Save price override", color = colors.primaryText)
            }
            error?.let { Text(it, style = AppTheme.caption, color = colors.dangerForeground) }
        }
    }
}

internal enum class UsageTab {
    USAGE,
    LIMITS,
}

@Composable
private fun UsageLimitPoolView(
    pool: dev.remoteagent.core.UsageLimitPool,
    useReset: (dev.remoteagent.core.UsageLimitAccount) -> Unit,
) {
    val colors = AppTheme.colors
    Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(pool.provider, style = AppTheme.title)
        pool.windows.forEach { window ->
            Row(Modifier.fillMaxWidth()) {
                Text(window.label, Modifier.weight(1f), style = AppTheme.body)
                Text("${window.remainingPercent}% remaining", style = AppTheme.caption)
            }
            LinearProgressIndicator(
                progress = { window.remainingPercent / 100f },
                modifier = Modifier.fillMaxWidth(),
                color = colors.primary,
                trackColor = colors.secondary,
            )
            window.pace?.let { pace -> Text("Pace: $pace", style = AppTheme.caption, color = colors.foregroundMuted) }
            window.resets.firstOrNull()?.let { reset ->
                Text(
                    "Next reset: ${reset.label} restores ${reset.restoresPercent}% at ${usageResetTime(reset.at)}",
                    style = AppTheme.caption,
                    color = colors.foregroundMuted,
                )
            }
            window.columns.forEach { column ->
                Row(Modifier.fillMaxWidth()) {
                    Text(column.label, Modifier.weight(1f), style = AppTheme.caption)
                    Text(
                        column.window?.let { "${it.remainingPercent}% left" } ?: "No limit reported",
                        style = AppTheme.caption,
                        color = colors.foregroundMuted,
                    )
                }
            }
        }
        pool.accounts
            .filter { it.resetCreditCount > 0 || it.externalLabel != null || it.error != null }
            .forEach { account -> UsageLimitAccount(account, showWindows = false) { useReset(account) } }
    }
}

@Composable
private fun UsageLimitAccount(
    account: dev.remoteagent.core.UsageLimitAccount,
    showWindows: Boolean = true,
    useReset: () -> Unit,
) {
    val colors = AppTheme.colors
    val context = LocalContext.current
    var confirmingReset by remember(account.id) { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(account.email ?: account.id, style = AppTheme.body)
        if (showWindows) {
            account.windows.forEach { window ->
                Row(Modifier.fillMaxWidth()) {
                    Text(window.label, Modifier.weight(1f), style = AppTheme.caption)
                    Text("${window.remainingPercent}% remaining", style = AppTheme.caption)
                }
                LinearProgressIndicator(
                    progress = { window.remainingPercent / 100f },
                    modifier = Modifier.fillMaxWidth(),
                    color = colors.primary,
                    trackColor = colors.secondary,
                )
            }
        }
        if (account.resetCreditCount > 0) {
            Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Text(
                    "Reset credits: ${account.resetCreditCount}",
                    style = AppTheme.caption,
                    color = colors.foregroundMuted,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = { confirmingReset = true }) { Text("Use reset", color = colors.primaryText) }
            }
        }
        account.externalLabel?.let { label ->
            TextButton(
                onClick = {
                    account.externalUrl?.let { url ->
                        val uri = Uri.parse(url)
                        if (uri.scheme == "https") {
                            context.startActivity(android.content.Intent(android.content.Intent.ACTION_VIEW, uri))
                        }
                    }
                }
            ) {
                Text(label, style = AppTheme.caption, color = colors.primaryText)
            }
        }
        account.error?.let { Text(it, style = AppTheme.caption, color = colors.warningForeground) }
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
                        useReset()
                    }
                ) {
                    Text("Use credit")
                }
            },
            dismissButton = { TextButton(onClick = { confirmingReset = false }) { Text("Cancel") } },
        )
    }
}

private fun usageResetTime(at: Long): String =
    java.time.Instant.ofEpochMilli(at)
        .atZone(java.time.ZoneId.systemDefault())
        .format(java.time.format.DateTimeFormatter.ofLocalizedDateTime(java.time.format.FormatStyle.SHORT))

private fun loadUsage(model: AndroidAppModel) {
    val now = java.time.LocalDate.now()
    val preferences = model.snapshot.usagePreferences()
    val input =
        UsageSummaryInput(
            sinceDay = now.minusDays(30).toString(),
            untilDay = now.toString(),
            timeZone = java.time.ZoneId.systemDefault().id,
            resolution = Resolution.DAY,
            sinceTime = null,
            untilTime = null,
            modelAliases = preferences.modelAliases,
            priceOverrides = preferences.priceOverrides,
        )
    model.perform(Intent.LoadUsageSummary(input))
    model.perform(Intent.LoadAccounts)
}

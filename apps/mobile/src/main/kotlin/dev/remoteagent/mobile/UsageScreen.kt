package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.ui.platform.LocalContext
import android.net.Uri
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Resolution
import dev.remoteagent.core.ComposerUsageLimits
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
internal fun UsageScreen(model: AndroidAppModel) {
    val view = model.snapshot.usagePage()
    var tab by remember { mutableStateOf(UsageTab.USAGE) }
    LaunchedEffect(Unit) { loadUsage(model) }
    ScreenScaffold("Usage", onBack = model::back, actions = {
        TextButton(onClick = { loadUsage(model) }) { Text("Refresh") }
        TextButton(onClick = {
            model.perform(Intent.RefreshUsageRates) { result ->
                if (result.isSuccess) loadUsage(model)
            }
        }) { Text("Refresh rates") }
    }) {
        LazyColumn(
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            item {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Center) {
                    TextButton(onClick = { tab = UsageTab.USAGE }) {
                        Text("Usage", color = if (tab == UsageTab.USAGE) AppTheme.colors.primaryText else AppTheme.colors.foregroundMuted)
                    }
                    TextButton(onClick = { tab = UsageTab.LIMITS }) {
                        Text("Limits", color = if (tab == UsageTab.LIMITS) AppTheme.colors.primaryText else AppTheme.colors.foregroundMuted)
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
                            view.error?.let { Text(it, style = AppTheme.caption, color = AppTheme.colors.dangerForeground) }
                        }
                    }
                }
                item { Text("Usage by model", style = AppTheme.title) }
                items(view.rows, key = { "${it.day}:${it.provider}:${it.model}" }) { row ->
                    Column(Modifier.fillMaxWidth().padding(horizontal = 8.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                        Text("${row.provider} · ${row.model}", style = AppTheme.body)
                        Text("${row.day} · ${row.tokens} tokens · ${row.costLabel}", style = AppTheme.caption, color = AppTheme.colors.foregroundMuted)
                    }
                }
                if (view.rows.isEmpty() && view.state == "ready") item {
                    Text("No usage in this period.", style = AppTheme.caption, color = AppTheme.colors.foregroundMuted)
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
            } else {
                item {
                    SectionCard("Limits") {
                        val accounts = model.snapshot.usageLimits()
                        if (accounts.isEmpty()) {
                            Text("Provider limits are unavailable until accounts are loaded.", Modifier.padding(16.dp), style = AppTheme.caption)
                        } else {
                            accounts.forEachIndexed { index, account ->
                                if (index > 0) HorizontalDivider(color = AppTheme.colors.border)
                                val source = model.snapshot.accounts()?.accounts?.firstOrNull { it.id == account.id }
                                UsageLimitAccount(account) {
                                    source?.let {
                                        model.perform(Intent.ConsumeResetCredit(it.provider, it.id, account.nextCreditId))
                                        model.perform(Intent.LoadAccounts)
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

private enum class UsageTab {
    USAGE,
    LIMITS,
}

@Composable
private fun UsageLimitAccount(account: dev.remoteagent.core.UsageLimitAccount, useReset: () -> Unit) {
    val colors = AppTheme.colors
    val context = LocalContext.current
    var confirmingReset by remember(account.id) { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(account.email ?: account.id, style = AppTheme.body)
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
        if (account.resetCreditCount > 0) {
            Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Text("Reset credits: ${account.resetCreditCount}", style = AppTheme.caption, color = colors.foregroundMuted, modifier = Modifier.weight(1f))
                TextButton(onClick = { confirmingReset = true }) { Text("Use reset", color = colors.primaryText) }
            }
        }
        account.externalLabel?.let { label ->
            TextButton(onClick = {
                account.externalUrl?.let { url ->
                    val uri = Uri.parse(url)
                    if (uri.scheme == "https") {
                        context.startActivity(android.content.Intent(android.content.Intent.ACTION_VIEW, uri))
                    }
                }
            }) { Text(label, style = AppTheme.caption, color = colors.primaryText) }
        }
        account.error?.let { Text(it, style = AppTheme.caption, color = colors.warningForeground) }
    }
    if (confirmingReset) {
        AlertDialog(
            onDismissRequest = { confirmingReset = false },
            title = { Text("Use a reset credit?") },
            text = { Text("This redeems one credit and clears the current rate-limit windows.") },
            confirmButton = {
                TextButton(onClick = { confirmingReset = false; useReset() }) { Text("Use credit") }
            },
            dismissButton = { TextButton(onClick = { confirmingReset = false }) { Text("Cancel") } },
        )
    }
}

private fun loadUsage(model: AndroidAppModel) {
    val now = java.time.LocalDate.now()
    val preferences = model.snapshot.usagePreferences()
    val input = UsageSummaryInput(
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

// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "CyclomaticComplexMethod", "LongParameterList", "MagicNumber")

package dev.remoteagent.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Star
import androidx.compose.material.icons.outlined.StarBorder
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.AgentRoster
import dev.remoteagent.core.ComposerView
import dev.remoteagent.core.Intent
import dev.remoteagent.core.LegacyModelsSection
import dev.remoteagent.core.ModelPickerRow
import dev.remoteagent.core.PickerRail
import dev.remoteagent.core.QueueAction
import dev.remoteagent.core.QueueRowView
import dev.remoteagent.core.QueueView
import dev.remoteagent.core.SetupCardView
import dev.remoteagent.core.SetupStageStatus
import dev.remoteagent.core.StatusTone
import dev.remoteagent.core.TraitControl

/** A Material bottom sheet with the Android sheet header. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun BottomSheet(
    onDismiss: () -> Unit,
    title: String,
    skipPartiallyExpanded: Boolean = false,
    leading: (@Composable () -> Unit)? = null,
    trailing: @Composable () -> Unit = {},
    content: @Composable ColumnScope.() -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = skipPartiallyExpanded),
        containerColor = AppTheme.colors.sheet,
        contentColor = AppTheme.colors.foreground,
    ) {
        Row(
            Modifier.fillMaxWidth().padding(start = if (leading != null) 4.dp else 20.dp, end = 8.dp, bottom = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            leading?.invoke()
            Text(title, Modifier.weight(1f), style = AppTheme.headline, fontWeight = FontWeight.ExtraBold)
            trailing()
        }
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).navigationBarsPadding()) { content() }
    }
}

@Composable
internal fun QueueSheet(model: AndroidAppModel, queue: QueueView, onDismiss: () -> Unit) {
    val colors = AppTheme.colors
    BottomSheet(onDismiss, queue.title) {
        queue.heldNotice?.let { notice ->
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(notice, Modifier.weight(1f), style = AppTheme.footnote, color = colors.foregroundSecondary)
                if (queue.canResume)
                    TextButton(onClick = { model.perform(Intent.Queue(QueueAction.Resume)) }) {
                        Text("Resume queue", color = colors.primaryText)
                    }
            }
        }
        queue.rows.forEachIndexed { index, row -> QueueRow(model, queue, row, index, onDismiss) }
    }
}

@Composable
private fun QueueRow(model: AndroidAppModel, queue: QueueView, row: QueueRowView, index: Int, onDismiss: () -> Unit) {
    val colors = AppTheme.colors
    val run = row.runId
    var menu by remember { mutableStateOf(false) }
    fun move(before: String?) {
        if (run != null) model.perform(Intent.Queue(QueueAction.Move(run, before)))
    }
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).background(colors.sheet).padding(start = 20.dp, end = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Column(Modifier.weight(1f).padding(vertical = 8.dp)) {
            if (row.editing)
                Text("EDITING", style = AppTheme.micro, fontWeight = FontWeight.Bold, color = colors.primaryText)
            Text(
                row.controls.displayText,
                style = AppTheme.footnote,
                color = colors.foreground,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (row.controls.canSteer && run != null)
            Box(
                Modifier.heightIn(min = 32.dp)
                    .background(colors.primary, CircleShape)
                    .clickable { model.perform(Intent.Queue(QueueAction.Steer(run))) }
                    .padding(horizontal = 10.5.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    "Steer",
                    style = AppTheme.caption,
                    fontWeight = FontWeight.Medium,
                    color = colors.primaryForeground,
                )
            }
        Box {
            IconButton(onClick = { menu = true }) {
                Icon(Icons.Outlined.MoreVert, row.actionsAccessibilityLabel, tint = colors.iconMuted)
            }
            AnchoredMenu(menu, { menu = false }) {
                if (row.controls.canSteer && run != null)
                    QueueMenuItem("Steer now") {
                        menu = false
                        model.perform(Intent.Queue(QueueAction.Steer(run)))
                    }
                if (row.controls.canEdit && run != null)
                    QueueMenuItem("Edit") {
                        menu = false
                        model.perform(Intent.Queue(QueueAction.Edit(run)))
                        onDismiss()
                    }
                if (row.controls.canMoveUp)
                    QueueMenuItem("Move up") {
                        menu = false
                        move(queue.rows.getOrNull(index - 1)?.runId)
                    }
                if (row.controls.canMoveDown)
                    QueueMenuItem("Move down") {
                        menu = false
                        move(queue.rows.getOrNull(index + 2)?.runId)
                    }
                if (row.controls.canDismiss && run != null)
                    QueueMenuItem("Remove", destructive = true) {
                        menu = false
                        model.perform(Intent.Queue(QueueAction.Cancel(run)))
                    }
            }
        }
    }
    HorizontalDivider(color = colors.border)
}

@Composable
private fun QueueMenuItem(label: String, destructive: Boolean = false, onClick: () -> Unit) {
    DropdownMenuItem(
        text = {
            Text(
                label,
                style = AppTheme.footnote,
                color = if (destructive) AppTheme.colors.dangerForeground else AppTheme.colors.foreground,
            )
        },
        onClick = onClick,
    )
}

@Composable
internal fun AgentsSheet(model: AndroidAppModel, roster: AgentRoster, onDismiss: () -> Unit) {
    val colors = AppTheme.colors
    BottomSheet(onDismiss, "Agents") {
        if (roster.rows.isEmpty())
            Text(
                "No agents in this turn.",
                Modifier.padding(20.dp),
                style = AppTheme.footnote,
                color = colors.foregroundMuted,
            )
        roster.rows.forEach { agent ->
            Row(
                Modifier.fillMaxWidth()
                    .clickable {
                        onDismiss()
                        model.openThread(agent.childThreadId)
                    }
                    .padding(horizontal = 20.dp, vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                ProviderIcon(agent.driver, 18.dp)
                Column(Modifier.weight(1f)) {
                    Text(
                        agent.title,
                        style = AppTheme.body,
                        fontWeight = FontWeight.Medium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        listOfNotNull(agent.modelLabel.takeIf { it.isNotEmpty() }, agent.detail).joinToString(" · "),
                        style = AppTheme.caption,
                        color = colors.foregroundMuted,
                        maxLines = 2,
                    )
                }
                Column(horizontalAlignment = Alignment.End) {
                    Text(
                        agent.statusLabel,
                        style = AppTheme.caption,
                        fontWeight = FontWeight.Medium,
                        color =
                            when (agent.tone) {
                                StatusTone.WORKING -> colors.sky
                                StatusTone.COMPLETED -> colors.emerald
                                StatusTone.FAILED -> colors.dangerForeground
                                StatusTone.INACTIVE -> colors.foregroundMuted
                            },
                    )
                    agent.elapsed?.let { Text(it, style = AppTheme.caption, color = colors.foregroundTertiary) }
                }
            }
            HorizontalDivider(color = colors.border)
        }
    }
}

/** Model list with a provider filter, then Options. A new model applies on Save. */
@Composable
internal fun ThreadSettingsSheet(model: AndroidAppModel, composer: ComposerView, onDismiss: () -> Unit) {
    val colors = AppTheme.colors
    var query by remember { mutableStateOf("") }
    var rail by remember { mutableStateOf<PickerRail?>(null) }
    var pending by remember { mutableStateOf<ModelPickerRow?>(null) }
    var toggledLegacy by remember { mutableStateOf(emptyList<String>()) }
    var choosing by remember { mutableStateOf<OptionScreen?>(null) }
    val picker by
        rememberView(model.snapshot, query, rail, toggledLegacy) { it.modelPicker(query, rail, toggledLegacy) }
    fun save() {
        pending?.let { row -> model.perform(Intent.SetModel(row.instanceId, row.driver, row.slug, emptyList())) }
        onDismiss()
    }
    val screen = choosing
    BottomSheet(
        onDismiss,
        screen?.title ?: "Thread settings",
        skipPartiallyExpanded = true,
        leading =
            screen?.let { { HeaderIconButton(Icons.AutoMirrored.Outlined.ArrowBack, "Back") { choosing = null } } },
        trailing = {
            if (screen == null)
                TextButton(onClick = ::save) {
                    Text(if (pending != null) "Save" else "Done", color = colors.foreground)
                }
        },
    ) {
        if (screen != null) {
            OptionChoices(screen) { choosing = null }
            return@BottomSheet
        }
        SettingsField(query, { query = it }, "Find a model", Modifier.padding(horizontal = 16.dp))
        picker?.let { view ->
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                FilterChip("All providers", rail == null) { rail = null }
                view.rail.forEach { item ->
                    FilterChip(item.label, rail == item.rail && rail != null) { rail = item.rail }
                }
            }
            view.emptyLabel?.let {
                Text(it, Modifier.padding(20.dp), style = AppTheme.footnote, color = colors.foregroundMuted)
            }
            var provider: String? = null
            view.rows.forEachIndexed { index, row ->
                view.legacy
                    ?.takeIf { it.currentCount.toInt() == index }
                    ?.let { legacy -> LegacyModelsRow(legacy) { toggledLegacy = toggledLegacy.toggled(it) } }
                if (row.providerName != provider) {
                    provider = row.providerName
                    Text(
                        row.providerName,
                        Modifier.padding(start = 20.dp, top = 14.dp, bottom = 4.dp),
                        style = AppTheme.label,
                        fontWeight = FontWeight.Medium,
                        color = colors.foregroundSecondary,
                    )
                }
                val selected = pending?.key?.let { it == row.key } ?: row.selected
                ChoiceRow(
                    row.name,
                    row.disabledReason,
                    selected,
                    enabled = row.disabledReason == null,
                    trailing = {
                        IconButton(onClick = { model.perform(Intent.ToggleFavoriteModel(row.instanceId, row.slug)) }) {
                            Icon(
                                if (row.favorite) Icons.Outlined.Star else Icons.Outlined.StarBorder,
                                if (row.favorite) "Remove from favorites: ${row.name}"
                                else "Add to favorites: ${row.name}",
                                tint = if (row.favorite) colors.icon else colors.iconMuted,
                            )
                        }
                    },
                ) {
                    pending = if (row.selected) null else row
                }
            }
            view.legacy
                ?.takeIf { it.currentCount.toInt() >= view.rows.size }
                ?.let { legacy -> LegacyModelsRow(legacy) { toggledLegacy = toggledLegacy.toggled(it) } }
        }
        SheetSection("Options")
        if (composer.traits.visible)
            composer.traits.controls.forEach { control ->
                when (control) {
                    is TraitControl.Select ->
                        DisclosureRow(
                            control.label,
                            control.note ?: control.choices.firstOrNull { it.id == control.selected }?.label.orEmpty(),
                            enabled = !control.disabled,
                        ) {
                            choosing =
                                OptionScreen(
                                    control.label,
                                    control.choices.map {
                                        OptionScreenChoice(it.label, null, it.id == control.selected)
                                    },
                                ) { index ->
                                    model.perform(Intent.SelectTrait(control.id, control.choices[index].id))
                                }
                        }
                    is TraitControl.Toggle ->
                        SwitchRow(control.label, control.on) { model.perform(Intent.ToggleTrait(control.id, it)) }
                }
            }
        val runtime = composer.controls
        DisclosureRow("Runtime", runtime.runtimeMode.label) {
            choosing =
                OptionScreen(
                    "Runtime",
                    runtime.runtimeModeChoices.map {
                        OptionScreenChoice(it.label, it.description, it.mode == runtime.runtimeMode.mode)
                    },
                ) { index ->
                    model.perform(Intent.SetRuntimeMode(runtime.runtimeModeChoices[index].mode))
                }
        }
        Spacer(Modifier.heightIn(min = 24.dp))
    }
}

private class OptionScreenChoice(val label: String, val description: String?, val selected: Boolean)

/** A descriptor's choices, pushed over the settings like a screen. */
private class OptionScreen(val title: String, val choices: List<OptionScreenChoice>, val select: (Int) -> Unit)

@Composable
private fun OptionChoices(screen: OptionScreen, onDone: () -> Unit) {
    screen.choices.forEachIndexed { index, choice ->
        ChoiceRow(choice.label, choice.description, choice.selected) {
            screen.select(index)
            onDone()
        }
    }
    Spacer(Modifier.heightIn(min = 24.dp))
}

/** An instance's legacy models, folded until the switch shows them. */
@Composable
private fun LegacyModelsRow(legacy: LegacyModelsSection, onToggle: (String) -> Unit) {
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 56.dp)
            .clickable { onToggle(legacy.instanceId) }
            .padding(horizontal = 20.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f).padding(vertical = 8.dp)) {
            Text(legacy.label, style = AppTheme.footnote, fontWeight = FontWeight.Medium)
            Text(legacy.detail, style = AppTheme.caption, color = AppTheme.colors.foregroundMuted)
        }
        AppSwitch(legacy.expanded) { onToggle(legacy.instanceId) }
    }
}

@Composable
private fun DisclosureRow(label: String, value: String, enabled: Boolean = true, onClick: () -> Unit) {
    val colors = AppTheme.colors
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 56.dp)
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 20.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(label, style = AppTheme.footnote, fontWeight = FontWeight.Medium, color = colors.foreground)
        Spacer(Modifier.weight(1f))
        Text(value, style = AppTheme.footnote, color = colors.foregroundMuted, maxLines = 1)
        Icon(Icons.Outlined.ChevronRight, null, Modifier.size(12.dp), tint = colors.iconMuted)
    }
}

@Composable
private fun SwitchRow(label: String, on: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 44.dp).padding(horizontal = 20.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, Modifier.weight(1f), style = AppTheme.footnote, fontWeight = FontWeight.Medium)
        AppSwitch(on, onChange = onChange)
    }
}

@Composable
internal fun AppSwitch(on: Boolean, enabled: Boolean = true, onChange: (Boolean) -> Unit) {
    Switch(
        on,
        onChange,
        enabled = enabled,
        colors =
            SwitchDefaults.colors(
                checkedTrackColor = AppTheme.colors.primary,
                checkedThumbColor = AppTheme.colors.primaryForeground,
                uncheckedTrackColor = AppTheme.colors.secondary,
                uncheckedThumbColor = AppTheme.colors.iconMuted,
            ),
    )
}

@Composable
private fun SheetSection(title: String) {
    Text(
        title,
        Modifier.padding(start = 20.dp, top = 18.dp, bottom = 4.dp),
        style = AppTheme.label,
        fontWeight = FontWeight.Medium,
        color = AppTheme.colors.foregroundSecondary,
    )
}

@Composable
private fun FilterChip(label: String, selected: Boolean, onClick: () -> Unit) {
    val colors = AppTheme.colors
    Text(
        label,
        Modifier.background(if (selected) colors.secondary else colors.screen, CircleShape)
            .border(1.dp, colors.border, CircleShape)
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 6.dp),
        style = AppTheme.caption,
        fontWeight = FontWeight.Medium,
        color = colors.foreground,
    )
}

/** Android settings rows: 56 tall, a leading radio and the secondary fill when selected. */
@Composable
internal fun ChoiceRow(
    label: String,
    detail: String?,
    selected: Boolean,
    enabled: Boolean = true,
    radio: Boolean = true,
    indent: Boolean = false,
    trailing: @Composable () -> Unit = {},
    onClick: () -> Unit,
) {
    val colors = AppTheme.colors
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 56.dp)
            .background(if (selected) colors.secondary else colors.sheet)
            .clickable(enabled = enabled, onClick = onClick)
            .padding(start = if (indent) 36.dp else 20.dp, end = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        if (radio) RadioIndicator(selected)
        Column(Modifier.weight(1f).padding(vertical = 8.dp)) {
            Text(
                label,
                style = AppTheme.body,
                color = if (enabled) colors.foreground else colors.foregroundMuted,
                maxLines = 2,
            )
            detail?.let { Text(it, style = AppTheme.caption, color = colors.foregroundMuted, maxLines = 2) }
        }
        trailing()
    }
}

@Composable
private fun RadioIndicator(selected: Boolean) {
    val colors = AppTheme.colors
    Box(
        Modifier.size(20.dp).border(2.dp, if (selected) colors.primary else colors.iconMuted, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        if (selected) Box(Modifier.size(10.dp).background(colors.primary, CircleShape))
    }
}

/** "Worktree setup" details: stages, the output tail, cancel, and the setup terminal. */
@Composable
internal fun SetupDetailsSheet(
    model: AndroidAppModel,
    card: SetupCardView,
    onOpenTerminal: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = AppTheme.colors
    BottomSheet(
        onDismiss,
        "Worktree setup",
        skipPartiallyExpanded = true,
        trailing = {
            TextButton(onClick = onDismiss) { Text("Done", style = AppTheme.footnote, color = colors.foreground) }
        },
    ) {
        Column(Modifier.padding(horizontal = 20.dp, vertical = 12.dp)) {
            card.stages.forEach { stage ->
                SetupStageLine(stage, compact = false)
                stage.output?.let { tail ->
                    Column(
                        Modifier.fillMaxWidth()
                            .padding(start = 32.dp, bottom = 8.dp)
                            .background(if (tail.failed) colors.danger else colors.cardAlt, RoundedCornerShape(8.dp))
                            .border(
                                1.dp,
                                if (tail.failed) colors.dangerBorder else colors.border,
                                RoundedCornerShape(8.dp),
                            )
                            .padding(horizontal = 12.dp, vertical = 8.dp)
                    ) {
                        for (slot in 0 until OUTPUT_TAIL_LINES) {
                            Text(
                                tail.lines.getOrNull(tail.lines.size - OUTPUT_TAIL_LINES + slot) ?: " ",
                                style = AppTheme.micro,
                                fontFamily = AppTheme.mono,
                                color = if (tail.failed) colors.dangerForeground else colors.foregroundSecondary,
                                maxLines = 1,
                            )
                        }
                    }
                }
            }
            card.error?.let {
                Text(
                    it,
                    Modifier.padding(start = 32.dp, top = 8.dp),
                    style = AppTheme.caption,
                    color = colors.dangerForeground,
                )
            }
            card.details.forEach { detail ->
                Row(Modifier.padding(top = 6.dp)) {
                    Text(detail.label, Modifier.width(96.dp), style = AppTheme.caption, color = colors.foregroundMuted)
                    Text(
                        detail.value,
                        style = AppTheme.caption,
                        fontFamily = AppTheme.mono,
                        color = colors.foregroundSecondary,
                    )
                }
            }
            val terminal = card.openTerminalId
            if (card.canCancel || card.canWorkLocally || terminal != null) {
                HorizontalDivider(Modifier.padding(top = 12.dp), color = colors.border)
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(16.dp, Alignment.End)) {
                    if (terminal != null)
                        TextButton(
                            onClick = {
                                onDismiss()
                                onOpenTerminal(terminal)
                            }
                        ) {
                            Text("Open terminal", style = AppTheme.footnote, color = colors.foreground)
                        }
                    if (card.canCancel)
                        TextButton(
                            onClick = {
                                onDismiss()
                                model.perform(Intent.CancelSetup)
                            }
                        ) {
                            Text("Cancel setup", style = AppTheme.footnote, color = colors.dangerForeground)
                        }
                    if (card.canWorkLocally)
                        TextButton(
                            onClick = {
                                onDismiss()
                                model.workLocally()
                            }
                        ) {
                            Text("Work locally", style = AppTheme.footnote, color = colors.foreground)
                        }
                }
            }
        }
    }
}

private const val OUTPUT_TAIL_LINES = 4

@Composable
internal fun SetupStageLine(stage: dev.remoteagent.core.SetupStageView, compact: Boolean) {
    val colors = AppTheme.colors
    val failed = stage.status == SetupStageStatus.FAILED
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = if (compact) 28.dp else 38.5.dp)
            .then(if (stage.status == SetupStageStatus.PENDING) Modifier.alphaOf(0.4f) else Modifier),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Box(Modifier.width(24.dp), contentAlignment = Alignment.Center) {
            Text(
                when (stage.status) {
                    SetupStageStatus.PENDING -> "○"
                    SetupStageStatus.RUNNING -> "◷"
                    SetupStageStatus.DONE -> "✓"
                    SetupStageStatus.SKIPPED -> "–"
                    SetupStageStatus.WARNING -> "!"
                    SetupStageStatus.FAILED -> "✕"
                },
                style = AppTheme.footnote,
                color =
                    when (stage.status) {
                        SetupStageStatus.FAILED -> colors.dangerForeground
                        SetupStageStatus.WARNING -> colors.warningForeground
                        else -> colors.iconMuted
                    },
            )
        }
        if (stage.status == SetupStageStatus.RUNNING) Shimmer(stage.label, Modifier.weight(1f))
        else
            Text(
                stage.label,
                Modifier.weight(1f),
                style = AppTheme.footnote,
                color = if (failed) colors.dangerForeground else colors.foregroundSecondary,
                maxLines = 1,
            )
        stage.trailing?.let { Text(it, style = AppTheme.micro, color = colors.foregroundSecondary, maxLines = 1) }
        stage.elapsed?.let { Text(it, style = AppTheme.micro, color = colors.foregroundSecondary) }
    }
}

private fun Modifier.alphaOf(value: Float) = alpha(value)

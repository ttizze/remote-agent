package dev.remoteagent.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.ComposerOptions
import dev.remoteagent.core.ComposerShortcuts
import dev.remoteagent.core.Intent
import dev.remoteagent.core.InteractionMode
import dev.remoteagent.core.NewThreadView

/** The new-task draft: the hero headline with its project picker, Plan/Build, and the composer. */
@Composable
internal fun NewTaskScreen(model: AndroidAppModel) {
    val options = remember {
        ComposerOptions(compact = true, alternateModifier = false, shortcuts = ComposerShortcuts(null, null, null))
    }
    val view by rememberView(model.snapshot) { it.newThread(options) }
    var settings by remember { mutableStateOf(false) }
    ScreenScaffold("New task", onBack = model::back) {
        val current = view ?: return@ScreenScaffold
        Column(Modifier.fillMaxSize().imePadding()) {
            Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                if (current.showHero) Hero(model, current)
            }
            if (current.composer.controls.interactionToggle != null) {
                Row(
                    Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    ModeChip("Build", current.composer.controls.interactionMode == InteractionMode.DEFAULT) {
                        model.perform(Intent.SetInteractionMode(InteractionMode.DEFAULT))
                    }
                    ModeChip("Plan", current.composer.controls.interactionMode == InteractionMode.PLAN) {
                        model.perform(Intent.SetInteractionMode(InteractionMode.PLAN))
                    }
                }
            }
            Composer(model, current.composer) { settings = true }
        }
        if (settings) ThreadSettingsSheet(model, current.composer) { settings = false }
    }
}

@Composable
private fun Hero(model: AndroidAppModel, view: NewThreadView) {
    val colors = AppTheme.colors
    var menu by remember { mutableStateOf(false) }
    var adding by remember { mutableStateOf(false) }
    Column(Modifier.padding(horizontal = 24.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Text(
            view.hero.headingLabel,
            style = AppTheme.largeTitle,
            fontWeight = FontWeight.Bold,
            color = colors.foreground,
            textAlign = TextAlign.Center,
        )
        if (view.hero.projectMenu || view.hero.projectChoices.isEmpty())
            Box(Modifier.padding(top = 14.dp)) {
                Row(
                    Modifier.border(1.dp, colors.border, CircleShape)
                        .clickable { menu = true }
                        .padding(horizontal = 14.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    Text(
                        view.hero.projectLabel,
                        style = AppTheme.footnote,
                        fontWeight = FontWeight.Medium,
                        color = colors.foreground,
                    )
                    Icon(Icons.Outlined.ExpandMore, null, Modifier.size(14.dp), tint = colors.iconMuted)
                }
                AnchoredMenu(menu, { menu = false }) {
                    view.hero.projectChoices.forEach { choice ->
                        DropdownMenuItem(
                            text = {
                                Text(
                                    if (choice.selected) "✓  ${choice.name}" else choice.name,
                                    style = AppTheme.footnote,
                                )
                            },
                            onClick = {
                                menu = false
                                model.perform(Intent.NewThread(choice.projectId))
                            },
                        )
                    }
                    DropdownMenuItem(
                        text = { Text("Add project…", style = AppTheme.footnote) },
                        onClick = {
                            menu = false
                            adding = true
                        },
                    )
                }
            }
    }
    if (adding) AddProjectDialog(model) { adding = false }
}

/** Registers a project folder on the Host by its absolute path. */
@Composable
private fun AddProjectDialog(model: AndroidAppModel, onDismiss: () -> Unit) {
    var path by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = AppTheme.colors.cardAlt,
        shape = RoundedCornerShape(28.dp),
        title = { Text("Add project", style = AppTheme.title) },
        text = { SettingsField(path, { path = it }, "Absolute path on the Host") },
        confirmButton = {
            TextButton(
                onClick = {
                    model.perform(Intent.AddProject(path.trim()))
                    onDismiss()
                },
                enabled = path.isNotBlank(),
            ) {
                Text("Add", color = AppTheme.colors.foreground)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel", color = AppTheme.colors.foreground) } },
    )
}

@Composable
private fun ModeChip(label: String, selected: Boolean, onClick: () -> Unit) {
    val colors = AppTheme.colors
    Text(
        label,
        Modifier.background(if (selected) colors.secondary else colors.screen, RoundedCornerShape(12.dp))
            .border(1.dp, colors.border, RoundedCornerShape(12.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 8.dp),
        style = AppTheme.footnote,
        fontWeight = FontWeight.Medium,
        color = if (selected) colors.foreground else colors.foregroundMuted,
    )
}

// Declarative native layout with the fixed mobile metrics; the project lists and path rules are supplied by core.
@file:Suppress("LongMethod", "MagicNumber", "TooManyFunctions")

package dev.remoteagent.mobile

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.CreateNewFolder
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.SubdirectoryArrowLeft
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.AddProjectTarget
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ProjectPickerEmpty
import dev.remoteagent.core.addProjectInitialQuery

/** "Choose project": "No project", the projects, or what to do when there are none. */
@Composable
internal fun ChooseProjectScreen(model: AndroidAppModel) {
    val colors = AppTheme.colors
    var query by remember { mutableStateOf("") }
    val view by rememberView(model.snapshot, query) { it.projectPicker(query) }
    ScreenScaffold(
        "Choose project",
        onBack = model::back,
        actions = {
            if (view?.canAddProject == true)
                HeaderIconButton(Icons.Outlined.Add, "Add project") { model.navigate(Route.AddProject) }
        },
    ) {
        val current = view ?: return@ScreenScaffold
        Column(Modifier.fillMaxSize()) {
            if (current.empty == null)
                SettingsField(
                    query,
                    { query = it },
                    "Search projects",
                    Modifier.padding(start = 16.dp, end = 16.dp, top = 4.dp),
                )
            val empty = current.empty
            if (empty != null) {
                ProjectsEmpty(model, empty)
                return@Column
            }
            LazyColumn(
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (current.noProject)
                    item(key = "no-project") {
                        ListCard {
                            ListRow(
                                "No project",
                                "Start a task without a project",
                                leading = { RowIcon(Icons.AutoMirrored.Outlined.Chat) },
                            ) {
                                model.chooseProject(null)
                            }
                        }
                    }
                if (current.noMatches)
                    item(key = "no-matches") {
                        Column(
                            Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 32.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            Text(
                                "No matching projects",
                                style = AppTheme.headline,
                                fontWeight = FontWeight.Bold,
                                color = colors.foreground,
                                textAlign = TextAlign.Center,
                            )
                            Text(
                                "Try a different project name or workspace path.",
                                style = AppTheme.footnote,
                                color = colors.foregroundMuted,
                                textAlign = TextAlign.Center,
                            )
                        }
                    }
                if (current.rows.isNotEmpty())
                    item(key = "projects") {
                        ListCard {
                            current.rows.forEach { row ->
                                ListRow(row.title, row.subtitle, leading = { ProjectFavicon(row.projectId, 24.dp) }) {
                                    model.chooseProject(row.projectId)
                                }
                            }
                        }
                    }
            }
        }
    }
}

@Composable
private fun ProjectsEmpty(model: AndroidAppModel, empty: ProjectPickerEmpty) {
    val colors = AppTheme.colors
    Box(Modifier.fillMaxSize().padding(16.dp), contentAlignment = Alignment.Center) {
        Column(
            Modifier.padding(horizontal = 24.dp, vertical = 32.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (empty.loading)
                CircularProgressIndicator(Modifier.size(24.dp), color = colors.iconMuted, strokeWidth = 2.dp)
            Text(
                empty.title,
                style = AppTheme.headline,
                fontWeight = FontWeight.Bold,
                color = colors.foreground,
                textAlign = TextAlign.Center,
            )
            Text(empty.detail, style = AppTheme.footnote, color = colors.foregroundMuted, textAlign = TextAlign.Center)
            Button(
                onClick = { if (empty.addProject) model.navigate(Route.AddProject) else model.openPairing() },
                colors =
                    ButtonDefaults.buttonColors(
                        containerColor = colors.primary,
                        contentColor = colors.primaryForeground,
                    ),
            ) {
                Text(if (empty.addProject) "Add new project" else "Add environment", style = AppTheme.footnote)
            }
            if (empty.startWithoutProject)
                FilledTonalButton(
                    onClick = { model.chooseProject(null) },
                    colors =
                        ButtonDefaults.filledTonalButtonColors(
                            containerColor = colors.secondary,
                            contentColor = colors.secondaryForeground,
                        ),
                ) {
                    Text("Start without a project", style = AppTheme.footnote)
                }
        }
    }
}

/** "Add project": where the project comes from. */
@Composable
internal fun AddProjectScreen(model: AndroidAppModel) {
    ScreenScaffold("Add project", onBack = model::back) {
        Column(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            ListCard {
                ListRow(
                    "Local folder",
                    "Browse a folder on disk",
                    leading = { RowIcon(Icons.Outlined.CreateNewFolder, muted = false) },
                ) {
                    model.navigate(Route.AddProjectLocal)
                }
            }
        }
    }
}

/** "Local folder": a path field, "Add project", and the folders under the typed path. */
@Composable
internal fun LocalFolderScreen(model: AndroidAppModel) {
    val colors = AppTheme.colors
    var path by remember { mutableStateOf(addProjectInitialQuery(null)) }
    var error by remember { mutableStateOf<String?>(null) }
    // The folder whose listing failed, and why.
    var failed by remember { mutableStateOf<Pair<String, String>?>(null) }
    var submitting by remember { mutableStateOf(false) }
    var existing by remember { mutableStateOf<AddProjectTarget.Existing?>(null) }
    val browser by rememberView(model.snapshot, path) { it.folderBrowser(path) }
    val directory = browser?.directoryPath
    val listFailure = failed?.takeIf { it.first == directory }?.second
    LaunchedEffect(directory, browser?.isBrowsing) {
        val target = browser
        if (target == null || !target.isBrowsing || target.listed) return@LaunchedEffect
        val requested = target.directoryPath
        model.perform(Intent.ListFiles(requested)) { result ->
            result.exceptionOrNull()?.let { failure ->
                model.notice = null
                failed = requested to (failure.message ?: "The folder could not be listed.")
            }
        }
    }
    fun submit() {
        if (submitting) return
        error = null
        when (val target = model.snapshot.addProjectTarget(path)) {
            is AddProjectTarget.Invalid -> error = target.message
            is AddProjectTarget.Existing -> existing = target
            is AddProjectTarget.Add -> {
                submitting = true
                model.perform(Intent.AddProject(target.path)) { result ->
                    submitting = false
                    result.exceptionOrNull()?.let { failure ->
                        model.notice = null
                        error = failure.message
                    } ?: model.snapshot.selectedProjectId()?.let(model::projectAdded)
                }
            }
        }
    }
    existing?.let { project ->
        AlertDialog(
            onDismissRequest = {
                existing = null
                model.projectAdded(project.projectId)
            },
            containerColor = colors.cardAlt,
            shape = RoundedCornerShape(28.dp),
            title = { Text("Project already exists", style = AppTheme.title) },
            text = { Text(project.title, style = AppTheme.footnote, color = colors.foregroundSecondary) },
            confirmButton = {
                TextButton(
                    onClick = {
                        existing = null
                        model.projectAdded(project.projectId)
                    }
                ) {
                    Text("OK", color = colors.foreground)
                }
            },
        )
    }
    ScreenScaffold("Local folder", onBack = model::back) {
        LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            error?.let { message -> item(key = "error") { ErrorBanner(message) } }
            item(key = "path") { SettingsField(path, { path = it }, "~/projects/my-app") }
            item(key = "add") { PrimaryButton("Add project", enabled = !submitting, onClick = ::submit) }
            item(key = "title") {
                Text(
                    "Browse folders",
                    Modifier.padding(horizontal = 16.dp),
                    style = AppTheme.footnote,
                    fontWeight = FontWeight.Medium,
                    color = colors.primaryText,
                )
            }
            listFailure?.let { message -> item(key = "list-error") { ErrorBanner(message) } }
            item(key = "folders") {
                val current = browser
                ListCard {
                    if (current?.isBrowsing == true && !current.listed && listFailure == null)
                        Box(Modifier.fillMaxWidth().padding(vertical = 20.dp), contentAlignment = Alignment.Center) {
                            CircularProgressIndicator(
                                Modifier.size(20.dp),
                                color = colors.iconMuted,
                                strokeWidth = 2.dp,
                            )
                        }
                    current?.parentQuery?.let { parent ->
                        ListRow("..", null, leading = { RowIcon(Icons.Outlined.SubdirectoryArrowLeft) }, chevron = false) {
                            path = parent
                        }
                    }
                    current?.entries?.forEach { entry ->
                        ListRow(entry.name, null, leading = { RowIcon(Icons.Outlined.Folder) }, chevron = false) {
                            path = entry.query
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ErrorBanner(message: String) {
    val colors = AppTheme.colors
    Surface(
        Modifier.fillMaxWidth(),
        color = colors.danger,
        shape = RoundedCornerShape(16.dp),
        border = BorderStroke(1.dp, colors.dangerBorder),
    ) {
        Text(
            message,
            Modifier.padding(horizontal = 14.dp, vertical = 12.dp),
            style = AppTheme.footnote,
            fontWeight = FontWeight.Medium,
            color = colors.dangerForeground,
        )
    }
}

@Composable
private fun ListCard(content: @Composable () -> Unit) {
    Surface(Modifier.fillMaxWidth(), color = AppTheme.colors.groupedCard, shape = RoundedCornerShape(28.dp)) {
        Column { content() }
    }
}

@Composable
private fun RowIcon(icon: ImageVector, muted: Boolean = true) {
    Icon(icon, null, Modifier.size(24.dp), tint = if (muted) AppTheme.colors.iconMuted else AppTheme.colors.icon)
}

/** The Material list row: a 24 leading icon, title and subtitle, and a chevron. */
@Composable
private fun ListRow(
    title: String,
    subtitle: String?,
    leading: @Composable () -> Unit,
    chevron: Boolean = true,
    onClick: () -> Unit,
) {
    val colors = AppTheme.colors
    Row(
        Modifier.fillMaxWidth()
            .clickable(onClick = onClick)
            .heightIn(min = if (subtitle.isNullOrEmpty()) 56.dp else 72.dp)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Box(Modifier.size(24.dp), contentAlignment = Alignment.Center) { leading() }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(title, style = AppTheme.body, color = colors.foreground, maxLines = 2)
            if (!subtitle.isNullOrEmpty())
                Text(subtitle, style = AppTheme.footnote, color = colors.foregroundMuted, maxLines = 2)
        }
        if (chevron) Icon(Icons.Outlined.ChevronRight, null, Modifier.size(18.dp), tint = colors.chevron)
    }
}

// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "CyclomaticComplexMethod", "LongParameterList", "MagicNumber")

package dev.remoteagent.mobile

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.AltRoute
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Undo
import androidx.compose.material.icons.outlined.Archive
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.DriveFileRenameOutline
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.MarkEmailUnread
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Tag
import androidx.compose.material.icons.outlined.Timer
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Driver
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.ThreadMenuConfirmation
import dev.remoteagent.core.ThreadMenuItem
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

private const val CLOCK_TICK_MILLIS = 1000L

/** Wall-clock milliseconds, advancing once a second while shown. */
@Composable
internal fun rememberNow(): Long {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(CLOCK_TICK_MILLIS)
            now = System.currentTimeMillis()
        }
    }
    return now
}

/** Builds a core view off the main thread; the last view stays until the next is ready. */
@Composable
internal fun <T> rememberView(snapshot: Snapshot, vararg keys: Any?, build: (Snapshot) -> T): State<T?> =
    produceState<T?>(null, snapshot, *keys) { value = withContext(Dispatchers.Default) { build(snapshot) } }

internal fun copyText(context: Context, text: String) {
    context.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("", text))
}

/** Material icons for the menu icon names core uses. */
internal fun menuIcon(name: String?): ImageVector? =
    when (name) {
        "archive" -> Icons.Outlined.Archive
        "circle-check" -> Icons.Outlined.CheckCircle
        "clock" -> Icons.Outlined.Schedule
        "copy" -> Icons.Outlined.ContentCopy
        "mail-open" -> Icons.Outlined.MarkEmailUnread
        "pencil" -> Icons.Outlined.DriveFileRenameOutline
        "pin",
        "pin-off" -> Icons.Outlined.PushPin
        "settings" -> Icons.Outlined.Settings
        "timer" -> Icons.Outlined.Timer
        "trash" -> Icons.Outlined.Delete
        "folder" -> Icons.Outlined.Folder
        "git-branch" -> Icons.AutoMirrored.Outlined.AltRoute
        "hash" -> Icons.Outlined.Tag
        "undo" -> Icons.AutoMirrored.Outlined.Undo
        else -> null
    }

@Composable
internal fun ProviderIcon(driver: Driver?, size: Dp = 14.dp, modifier: Modifier = Modifier) {
    if (driver == null) return
    Icon(
        painterResource(if (driver == Driver.CLAUDE) R.drawable.ic_claude else R.drawable.ic_openai),
        driver.name,
        modifier.size(size),
        tint = AppTheme.colors.iconMuted,
    )
}

/** The Android toolbar: back, title and subtitle, and trailing icon buttons. */
@Composable
internal fun ScreenHeader(
    title: String,
    subtitle: String?,
    onBack: (() -> Unit)?,
    actions: @Composable () -> Unit = {},
) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 64.dp).padding(horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (onBack != null) HeaderIconButton(Icons.AutoMirrored.Outlined.ArrowBack, "Back", onClick = onBack)
        else Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f).padding(horizontal = 4.dp)) {
            Text(
                title,
                style = AppTheme.headline,
                fontWeight = FontWeight.ExtraBold,
                color = AppTheme.colors.headerForeground,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (!subtitle.isNullOrEmpty())
                Text(
                    subtitle,
                    style = AppTheme.caption,
                    color = AppTheme.colors.foregroundSecondary,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
        }
        actions()
    }
}

@Composable
internal fun HeaderIconButton(
    icon: ImageVector,
    label: String,
    selected: Boolean = false,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    IconButton(onClick = onClick, enabled = enabled, modifier = Modifier.size(48.dp)) {
        Box(
            Modifier.size(40.dp)
                .background(if (selected) AppTheme.colors.secondary else Color.Transparent, CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            Icon(icon, label, tint = AppTheme.colors.headerForeground, modifier = Modifier.size(24.dp))
        }
    }
}

@Composable
internal fun PrimaryButton(label: String, enabled: Boolean = true, modifier: Modifier = Modifier, onClick: () -> Unit) {
    RequestButton(label, RequestTone.Primary, enabled, modifier.fillMaxWidth(), large = true, onClick = onClick)
}

internal enum class RequestTone {
    Primary,
    Secondary,
    Danger,
}

/** The primary actions in cards that replace the composer. */
@Composable
internal fun RequestButton(
    label: String,
    tone: RequestTone,
    enabled: Boolean = true,
    modifier: Modifier = Modifier,
    large: Boolean = false,
    onClick: () -> Unit,
) {
    val colors = AppTheme.colors
    val (container, content) =
        when (tone) {
            RequestTone.Primary -> colors.primary to colors.primaryForeground
            RequestTone.Danger -> colors.danger to colors.dangerForeground
            RequestTone.Secondary -> colors.subtleStrong to colors.foreground
        }
    Surface(
        onClick = onClick,
        enabled = enabled,
        modifier = modifier,
        shape = RoundedCornerShape(if (large) 16.dp else 14.dp),
        color = container,
        contentColor = content,
    ) {
        Box(
            Modifier.padding(horizontal = if (large) 16.dp else 14.dp, vertical = if (large) 14.dp else 12.dp)
                .then(if (enabled) Modifier else Modifier.background(Color.Transparent)),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                label,
                style = AppTheme.footnote,
                fontWeight = if (tone == RequestTone.Primary) FontWeight.ExtraBold else FontWeight.Bold,
                color = if (enabled) content else content.copy(alpha = 0.5f),
            )
        }
    }
}

@Composable
internal fun SettingsField(
    value: String,
    onChange: (String) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
    minLines: Int = 1,
    singleLine: Boolean = minLines == 1,
) {
    Surface(
        modifier.fillMaxWidth(),
        color = AppTheme.colors.input,
        shape = RoundedCornerShape(16.dp),
        border = BorderStroke(1.dp, AppTheme.colors.inputBorder),
    ) {
        BasicTextField(
            value,
            onChange,
            Modifier.padding(horizontal = 16.dp, vertical = 14.dp),
            textStyle = AppTheme.body.copy(color = AppTheme.colors.foreground),
            minLines = minLines,
            singleLine = singleLine,
            cursorBrush = SolidColor(AppTheme.colors.primary),
            decorationBox = { input ->
                Box {
                    if (value.isEmpty()) Text(placeholder, style = AppTheme.body, color = AppTheme.colors.placeholder)
                    input()
                }
            },
        )
    }
}

/** A grouped settings card: rounded 28 on Android. */
@Composable
internal fun SectionCard(title: String?, modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (title != null)
            Text(
                title,
                Modifier.padding(horizontal = 16.dp),
                style = AppTheme.label,
                fontWeight = FontWeight.Medium,
                color = AppTheme.colors.foregroundSecondary,
            )
        Surface(color = AppTheme.colors.groupedCard, shape = RoundedCornerShape(28.dp)) { Column { content() } }
    }
}

/** Renders core menu items, asking for confirmation where the item requires it. */
@Composable
internal fun ThreadMenuItems(
    items: List<ThreadMenuItem>,
    onSelect: (ThreadMenuItem) -> Unit,
    onChildren: (ThreadMenuItem) -> Unit,
) {
    items.forEach { item ->
        if (item.separatorBefore) HorizontalDivider(color = AppTheme.colors.border)
        DropdownMenuItem(
            text = {
                Text(
                    item.label,
                    style = AppTheme.footnote,
                    color = if (item.destructive) AppTheme.colors.dangerForeground else AppTheme.colors.foreground,
                )
            },
            leadingIcon =
                menuIcon(item.icon)?.let { icon ->
                    {
                        Icon(
                            icon,
                            null,
                            tint = if (item.destructive) AppTheme.colors.dangerForeground else AppTheme.colors.icon,
                        )
                    }
                },
            enabled = item.enabled,
            onClick = { if (item.children.isNotEmpty()) onChildren(item) else onSelect(item) },
        )
    }
}

@Composable
internal fun AnchoredMenu(expanded: Boolean, onDismiss: () -> Unit, content: @Composable () -> Unit) {
    DropdownMenu(expanded, onDismiss, containerColor = AppTheme.colors.cardAlt, shape = RoundedCornerShape(16.dp)) {
        content()
    }
}

@Composable
internal fun ConfirmDialog(
    confirmation: ThreadMenuConfirmation,
    confirmLabel: String,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = AppTheme.colors.cardAlt,
        shape = RoundedCornerShape(28.dp),
        title = confirmation.title?.let { title -> { Text(title, style = AppTheme.title) } },
        text = { Text(confirmation.message, style = AppTheme.footnote, color = AppTheme.colors.foregroundSecondary) },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text(
                    confirmLabel,
                    color =
                        if (confirmation.destructive) AppTheme.colors.dangerForeground else AppTheme.colors.foreground,
                )
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel", color = AppTheme.colors.foreground) } },
    )
}

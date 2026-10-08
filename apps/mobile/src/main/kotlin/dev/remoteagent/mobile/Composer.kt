// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "CyclomaticComplexMethod", "LongParameterList", "MagicNumber")

package dev.remoteagent.mobile

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.FormatListBulleted
import androidx.compose.material.icons.automirrored.outlined.InsertDriveFile
import androidx.compose.material.icons.automirrored.outlined.Undo
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.ArrowUpward
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.GridView
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Stop
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material.icons.outlined.ViewInAr
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.ComposerCommandItem
import dev.remoteagent.core.ComposerCommandTarget
import dev.remoteagent.core.ComposerMenuView
import dev.remoteagent.core.ComposerPrimaryAction
import dev.remoteagent.core.ComposerTriggerKind
import dev.remoteagent.core.ComposerView
import dev.remoteagent.core.FollowUpBehavior
import dev.remoteagent.core.Intent
import dev.remoteagent.core.MobileSendIcon
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.QueueAction
import dev.remoteagent.core.SkillSourceKind
import dev.remoteagent.core.TimelineLayout

private fun followUpLabel(behavior: FollowUpBehavior) =
    when (behavior) {
        FollowUpBehavior.QUEUE -> "Queue"
        FollowUpBehavior.STEER -> "Steer"
        FollowUpBehavior.RESTART -> "Restart"
    }

private fun sendIcon(icon: MobileSendIcon): ImageVector =
    when (icon) {
        MobileSendIcon.ARROW_UP -> Icons.Outlined.ArrowUpward
        MobileSendIcon.CHECKMARK -> Icons.Outlined.Check
        MobileSendIcon.LIST_NUMBER -> Icons.AutoMirrored.Outlined.FormatListBulleted
        MobileSendIcon.ARROW_TURN_LEFT_UP -> Icons.AutoMirrored.Outlined.Undo
    }

/**
 * The thread composer: a capsule while idle and a card while editing. Text edits go through `DraftRevision` so an older
 * receipt never overwrites newer typing.
 */
@Composable
internal fun Composer(model: AndroidAppModel, composer: ComposerView, onOpenSettings: () -> Unit) {
    val colors = AppTheme.colors
    var value by remember(composer.draftKey) { mutableStateOf(TextFieldValue(model.composerText)) }
    var focused by remember { mutableStateOf(false) }
    val focusRequester = remember { FocusRequester() }
    LaunchedEffect(model.composerText) {
        if (value.text != model.composerText)
            value = TextFieldValue(model.composerText, TextRange(model.composerText.length))
    }
    LaunchedEffect(model.composerFocusRequests) {
        if (model.composerFocusRequests > 0) {
            value = TextFieldValue(model.composerText, TextRange(model.composerText.length))
            focusRequester.requestFocus()
        }
    }
    val expanded = focused || value.text.isNotEmpty() || composer.attachments.isNotEmpty()
    LaunchedEffect(composer.draftKey, value.text, value.selection) {
        model.perform(Intent.UpdateComposerMenu(value.text, value.selection.end.toUInt(), TimelineLayout.MOBILE))
    }
    val menu by
        rememberView(model.snapshot, value.text, value.selection) {
            if (value.selection.collapsed) it.composerMenu(value.text, value.selection.end.toUInt()) else null
        }
    val (dictation, startDictation) = rememberDictation(model, composer.draftKey)
    val voice = dictation.presentation
    // The mic, then Stop or Send unless dictation holds the send.
    val trailing: @Composable () -> Unit = {
        DictationPrimaryAction(dictation, available = model.snapshot.connected(), onStart = startDictation)
        if (voice.showsSend || composer.mobileShowsStop) PrimaryAction(model, composer)
    }
    val dictating: @Composable () -> Unit = {
        Row(Modifier.fillMaxWidth().padding(horizontal = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            DictationCancelAction(dictation)
            DictationStatus(dictation, Modifier.weight(1f))
            trailing()
        }
    }
    Column(
        Modifier.fillMaxWidth()
            .background(colors.composerPanel)
            .padding(horizontal = 12.dp, vertical = if (expanded) 8.dp else 6.dp)
    ) {
        if (composer.editingQueuedRun != null) EditingBanner(model)
        menu
            ?.takeIf {
                it.trigger != null && (it.items.isNotEmpty() || it.trigger?.kind == ComposerTriggerKind.PULL_REQUEST)
            }
            ?.let { current ->
                CommandPopover(current) { item ->
                    model.perform(Intent.SelectComposerItem(value.text, value.selection.end.toUInt(), item.id)) { result
                        ->
                        (result.getOrNull() as? Outcome.ComposerEdited)?.let { edited ->
                            value = TextFieldValue(model.composerText, TextRange(edited.cursor.toInt()))
                        }
                    }
                }
            }
        (composer.validationMessage ?: composer.attachmentError)?.let {
            Text(
                it,
                Modifier.padding(horizontal = 12.dp, vertical = 6.dp),
                style = AppTheme.caption,
                color = colors.dangerForeground,
            )
        }
        Surface(
            Modifier.fillMaxWidth(),
            shape = RoundedCornerShape(if (expanded) 26.dp else 27.dp),
            color = colors.composerSurface,
            border = BorderStroke(1.dp, colors.composerBorder),
        ) {
            val editor: @Composable (Modifier) -> Unit = { modifier ->
                BasicTextField(
                    value,
                    { next ->
                        val changed = next.text != value.text
                        value = next
                        if (changed) model.editDraft(next.text)
                    },
                    modifier.focusRequester(focusRequester).onFocusChanged { focused = it.isFocused },
                    enabled = !composer.editor.disabled,
                    readOnly = voice.blocksSubmission,
                    textStyle = AppTheme.body.copy(color = colors.foreground),
                    cursorBrush = SolidColor(colors.primary),
                    maxLines = if (expanded) Int.MAX_VALUE else 1,
                    decorationBox = { input ->
                        Box(contentAlignment = Alignment.CenterStart) {
                            if (value.text.isEmpty())
                                Text(
                                    composer.editor.placeholder,
                                    style = AppTheme.body,
                                    color = colors.placeholder,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            input()
                        }
                    },
                )
            }
            if (expanded)
                Column(Modifier.heightIn(min = 140.dp).padding(top = 14.dp, bottom = 6.dp)) {
                    if (composer.attachments.isNotEmpty())
                        Box(Modifier.padding(start = 14.dp, end = 14.dp, bottom = 10.dp)) {
                            DraftAttachmentStrip(model, composer.attachments, null)
                        }
                    editor(Modifier.fillMaxWidth().heightIn(min = 72.dp, max = 160.dp).padding(horizontal = 14.dp))
                    if (voice.status != null) dictating()
                    else
                        Row(
                            Modifier.fillMaxWidth().padding(horizontal = 6.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(6.dp),
                        ) {
                            AttachmentButton(model, composer.draftKey, enabled = !composer.editor.disabled)
                            ModelControl(composer, onOpenSettings)
                            Spacer(Modifier.weight(1f))
                            trailing()
                        }
                }
            else if (voice.status != null) Box(Modifier.padding(vertical = 2.dp)) { dictating() }
            else
                Row(
                    Modifier.padding(vertical = 2.dp, horizontal = 4.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    AttachmentButton(model, composer.draftKey, enabled = !composer.editor.disabled)
                    editor(Modifier.weight(1f).heightIn(min = 36.dp).padding(horizontal = 4.dp))
                    trailing()
                }
        }
    }
}

@Composable
private fun EditingBanner(model: AndroidAppModel) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            "EDITING",
            style = AppTheme.micro,
            fontWeight = FontWeight.Bold,
            color = AppTheme.colors.primaryText,
            modifier = Modifier.padding(end = 8.dp),
        )
        Text(
            "Editing queued message",
            Modifier.weight(1f),
            style = AppTheme.caption,
            color = AppTheme.colors.foregroundMuted,
        )
        Text(
            "Cancel",
            Modifier.clickable { model.perform(Intent.Queue(QueueAction.CancelEdit)) },
            style = AppTheme.caption,
            fontWeight = FontWeight.Medium,
            color = AppTheme.colors.foreground,
        )
    }
}

private fun menuGroupLabel(kind: ComposerTriggerKind?) =
    when (kind) {
        ComposerTriggerKind.PULL_REQUEST -> "Pull requests"
        ComposerTriggerKind.SLASH_COMMAND -> "Commands"
        ComposerTriggerKind.SKILL -> "Skills"
        ComposerTriggerKind.PATH -> "Files"
        ComposerTriggerKind.SLASH_MODEL,
        null -> null
    }

private fun skillIcon(source: SkillSourceKind?): ImageVector =
    when (source) {
        SkillSourceKind.APP -> Icons.Outlined.GridView
        SkillSourceKind.REPO,
        SkillSourceKind.PROJECT -> Icons.Outlined.Folder
        SkillSourceKind.PERSONAL -> Icons.Outlined.AccountCircle
        SkillSourceKind.SYSTEM -> Icons.Outlined.Settings
        SkillSourceKind.OTHER,
        null -> Icons.Outlined.ViewInAr
    }

private fun menuItemIcon(item: ComposerCommandItem): ImageVector =
    when (val target = item.target) {
        is ComposerCommandTarget.BuiltIn,
        is ComposerCommandTarget.ProviderCommand -> Icons.Outlined.Terminal
        is ComposerCommandTarget.Skill -> skillIcon(item.skillSource)
        is ComposerCommandTarget.Path ->
            if (target.directory) Icons.Outlined.Folder else Icons.AutoMirrored.Outlined.InsertDriveFile
        is ComposerCommandTarget.Thread -> Icons.Outlined.ChatBubbleOutline
    }

private const val SLASH_SKILL_PREFIX = "skill:"

/** The `/`, `$` and `@` menu above the composer card. */
@Composable
private fun CommandPopover(menu: ComposerMenuView, onSelect: (ComposerCommandItem) -> Unit) {
    val colors = AppTheme.colors
    val kind = menu.trigger?.kind
    Surface(
        Modifier.fillMaxWidth().padding(bottom = 8.dp),
        shape = RoundedCornerShape(16.dp),
        color = colors.cardAlt,
        border = BorderStroke(1.dp, colors.border),
    ) {
        Column {
            menuGroupLabel(kind)?.let { label ->
                Text(
                    label.uppercase(),
                    Modifier.padding(start = 14.dp, end = 14.dp, top = 10.dp, bottom = 4.dp),
                    style = AppTheme.micro,
                    fontWeight = FontWeight.Bold,
                    letterSpacing = 0.8.sp,
                    color = colors.foregroundMuted,
                )
            }
            if (menu.items.isEmpty())
                menu.emptyLabel?.let {
                    Text(
                        it,
                        Modifier.padding(horizontal = 14.dp, vertical = 10.dp),
                        style = AppTheme.label,
                        color = colors.foregroundTertiary,
                    )
                }
            else
                Column(Modifier.heightIn(max = 180.dp).verticalScroll(rememberScrollState())) {
                    menu.items.forEachIndexed { index, item ->
                        CommandRow(item, kind, onSelect)
                        if (index < menu.items.lastIndex) HorizontalDivider(thickness = 0.5.dp, color = colors.border)
                    }
                }
        }
    }
}

@Composable
private fun CommandRow(item: ComposerCommandItem, kind: ComposerTriggerKind?, onSelect: (ComposerCommandItem) -> Unit) {
    val colors = AppTheme.colors
    val path = item.target is ComposerCommandTarget.Path
    Row(
        Modifier.fillMaxWidth().clickable { onSelect(item) }.padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Icon(menuItemIcon(item), null, Modifier.size(if (path) 16.dp else 14.dp), tint = colors.iconMuted)
        val slashSkill =
            kind == ComposerTriggerKind.SLASH_COMMAND &&
                item.target is ComposerCommandTarget.Skill &&
                item.label.startsWith(SLASH_SKILL_PREFIX)
        Text(
            if (slashSkill)
                buildAnnotatedString {
                    withStyle(SpanStyle(color = colors.foregroundMuted)) { append(SLASH_SKILL_PREFIX) }
                    append(item.label.removePrefix(SLASH_SKILL_PREFIX))
                }
            else AnnotatedString(item.label),
            style = AppTheme.body,
            fontWeight = FontWeight.Medium,
            color = colors.foreground,
            maxLines = 1,
        )
        if (item.description.isNotEmpty())
            Text(
                item.description,
                Modifier.weight(1f),
                style = AppTheme.label,
                color = colors.foregroundMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
    }
}

/** Provider icon and model label; opens Thread settings. */
@Composable
internal fun ModelControl(composer: ComposerView, onClick: () -> Unit) {
    Row(
        Modifier.heightIn(min = 44.dp)
            .widthIn(max = 190.dp)
            .clip12()
            .clickable(onClick = onClick)
            .padding(horizontal = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        ProviderIcon(composer.modelTrigger.instance?.driver ?: composer.controls.model?.driver, 14.dp)
        Text(
            composer.modelTrigger.label,
            Modifier.weight(1f, fill = false),
            style = AppTheme.footnote,
            fontWeight = FontWeight.Medium,
            color = AppTheme.colors.foregroundMuted,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        Icon(Icons.Outlined.ExpandMore, null, Modifier.size(12.dp), tint = AppTheme.colors.iconMuted)
    }
}

private fun Modifier.clip12() = clip(RoundedCornerShape(12.dp))

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun PrimaryAction(model: AndroidAppModel, composer: ComposerView) {
    val colors = AppTheme.colors
    when (val action = composer.primaryAction) {
        is ComposerPrimaryAction.Implement -> ImplementPill(model, action)
        is ComposerPrimaryAction.Refine ->
            LabeledPill(action.label, !action.disabled) { model.perform(Intent.PlanFollowUp(false)) }
        is ComposerPrimaryAction.Answer ->
            LabeledPill(action.submitLabel, !action.submitDisabled) { model.perform(Intent.Send(false)) }
        else -> {
            if (composer.mobileShowsStop) {
                CircleButton(Icons.Outlined.Stop, "Stop", colors.danger, colors.dangerForeground, enabled = true) {
                    model.perform(Intent.Stop)
                }
                return
            }
            val send = composer.mobileSend
            val disabled = (action as? ComposerPrimaryAction.Send)?.v1?.disabled ?: false
            var menu by remember { mutableStateOf(false) }
            Box {
                CircleButton(
                    sendIcon(send.icon),
                    send.label,
                    if (disabled) colors.primary.copy(alpha = 0.15f) else colors.primary,
                    colors.primaryForeground,
                    enabled = !disabled,
                    onLongClick = if (send.alternate != null) ({ menu = true }) else null,
                ) {
                    model.perform(Intent.Send(false))
                }
                AnchoredMenu(menu, { menu = false }) {
                    send.alternate?.let { alternate ->
                        DropdownMenuItem(
                            text = { Text(followUpLabel(alternate), style = AppTheme.footnote) },
                            onClick = {
                                menu = false
                                model.perform(Intent.Send(true))
                            },
                        )
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun CircleButton(
    icon: ImageVector,
    label: String,
    container: androidx.compose.ui.graphics.Color,
    content: androidx.compose.ui.graphics.Color,
    enabled: Boolean,
    onLongClick: (() -> Unit)? = null,
    onClick: () -> Unit,
) {
    Box(
        Modifier.size(44.dp).combinedClickable(enabled = enabled, onClick = onClick, onLongClick = onLongClick),
        contentAlignment = Alignment.Center,
    ) {
        Box(Modifier.size(30.dp).background(container, CircleShape), contentAlignment = Alignment.Center) {
            Icon(icon, label, Modifier.size(16.dp), tint = content)
        }
    }
}

@Composable
private fun LabeledPill(label: String, enabled: Boolean, onClick: () -> Unit) {
    Surface(
        onClick = onClick,
        enabled = enabled,
        shape = CircleShape,
        color = if (enabled) AppTheme.colors.primary else AppTheme.colors.primary.copy(alpha = 0.15f),
        contentColor = AppTheme.colors.primaryForeground,
    ) {
        Text(
            label,
            Modifier.padding(horizontal = 14.dp, vertical = 8.dp),
            style = AppTheme.footnote,
            fontWeight = FontWeight.Bold,
            color = AppTheme.colors.primaryForeground,
        )
    }
}

@Composable
private fun ImplementPill(model: AndroidAppModel, action: ComposerPrimaryAction.Implement) {
    var menu by remember { mutableStateOf(false) }
    Box {
        Row(verticalAlignment = Alignment.CenterVertically) {
            LabeledPill(action.label, !action.disabled) { model.perform(Intent.PlanFollowUp(false)) }
            CircleButton(
                Icons.Outlined.ExpandMore,
                action.newThreadLabel,
                AppTheme.colors.secondary,
                AppTheme.colors.secondaryForeground,
                enabled = !action.disabled,
            ) {
                menu = true
            }
        }
        AnchoredMenu(menu, { menu = false }) {
            DropdownMenuItem(
                text = { Text(action.newThreadLabel, style = AppTheme.footnote) },
                onClick = {
                    menu = false
                    model.perform(Intent.PlanFollowUp(true))
                },
            )
        }
    }
}

// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "CyclomaticComplexMethod", "LongParameterList", "MagicNumber")

package dev.remoteagent.mobile

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Stop
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.AnswerEdit
import dev.remoteagent.core.ApprovalTone
import dev.remoteagent.core.ApprovalView
import dev.remoteagent.core.Intent
import dev.remoteagent.core.QuestionView
import dev.remoteagent.core.QuestionsView
import dev.remoteagent.core.answerDraftKey

private val CARD_SHAPE = RoundedCornerShape(20.dp)

@Composable
private fun Eyebrow(text: String) {
    Text(
        text.uppercase(),
        style = AppTheme.caption,
        fontWeight = FontWeight.Bold,
        letterSpacing = 1.1.sp,
        color = AppTheme.colors.foregroundSecondary,
    )
}

@Composable
private fun RequestCard(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    Surface(
        modifier.fillMaxWidth(),
        shape = CARD_SHAPE,
        color = AppTheme.colors.cardAlt,
        border = BorderStroke(1.dp, AppTheme.colors.border),
    ) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(8.75.dp)) { content() }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ApprovalCard(model: AndroidAppModel, approval: ApprovalView) {
    val colors = AppTheme.colors
    RequestCard {
        Eyebrow("Approval needed")
        Text(approval.cardTitle, style = AppTheme.headline, fontWeight = FontWeight.Bold, color = colors.foreground)
        approval.cardDetail?.let { detail ->
            Text(
                detail,
                Modifier.heightIn(max = 160.dp).verticalScroll(rememberScrollState()),
                style = AppTheme.footnote,
                fontFamily = if (approval.detailMonospace) AppTheme.mono else AppTheme.fonts,
                color = colors.foregroundSecondary,
            )
        }
        approval.unavailableNotice?.let { Text(it, style = AppTheme.footnote, color = colors.foregroundMuted) }
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.75.dp),
            verticalArrangement = Arrangement.spacedBy(8.75.dp),
        ) {
            approval.cardActions.forEach { action ->
                RequestButton(
                    action.label,
                    when (action.tone) {
                        ApprovalTone.PRIMARY -> RequestTone.Primary
                        ApprovalTone.DANGER -> RequestTone.Danger
                        ApprovalTone.SECONDARY -> RequestTone.Secondary
                    },
                    enabled = action.enabled && approval.canRespond && !approval.responding,
                ) {
                    model.perform(Intent.RespondApproval(approval.requestId, action.decision))
                }
            }
        }
    }
}

/** Collapsed: a pill bar. Expanded: every question with its options and answer field. */
@Composable
internal fun QuestionsCard(model: AndroidAppModel, questions: QuestionsView, canStop: Boolean) {
    var collapsed by rememberSaveable(questions.requestId) { mutableStateOf(false) }
    val colors = AppTheme.colors
    val count = questions.questionCount.toInt()
    val countLabel = "$count question${if (count == 1) "" else "s"}"
    if (collapsed) {
        Row(
            Modifier.fillMaxWidth()
                .background(colors.cardAlt, CircleShape)
                .border(1.dp, colors.border, CircleShape)
                .padding(start = 14.dp, end = 5.25.dp, top = 5.25.dp, bottom = 5.25.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                Modifier.weight(1f).heightIn(min = 40.dp).clickable { collapsed = false },
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(7.dp),
            ) {
                Eyebrow("User input needed")
                Text(countLabel, style = AppTheme.caption, color = colors.foregroundMuted)
                Spacer(Modifier.weight(1f))
                Icon(Icons.Outlined.ExpandLess, null, Modifier.size(16.dp), tint = colors.iconMuted)
            }
            if (canStop)
                IconButton(
                    onClick = { model.perform(Intent.Stop) },
                    modifier = Modifier.size(36.dp).background(colors.danger, CircleShape),
                ) {
                    Icon(Icons.Outlined.Stop, "Stop", tint = colors.dangerForeground, modifier = Modifier.size(16.dp))
                }
        }
        return
    }
    val disabled = !questions.canRespond || questions.responding
    RequestCard(Modifier.heightIn(max = 520.dp)) {
        Row(Modifier.fillMaxWidth().clickable { collapsed = true }, verticalAlignment = Alignment.Top) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.75.dp)) {
                Eyebrow("User input needed")
                Text(
                    "Fill in the pending answers",
                    style = AppTheme.headline,
                    fontWeight = FontWeight.Bold,
                    color = colors.foreground,
                )
            }
            Box(
                Modifier.size(32.dp).background(colors.subtleStrong, CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Icon(Icons.Outlined.ExpandMore, "Collapse user input", Modifier.size(14.dp), tint = colors.iconMuted)
            }
        }
        Column(
            Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(8.75.dp),
        ) {
            questions.unavailableNotice?.let { Text(it, style = AppTheme.footnote, color = colors.foregroundMuted) }
            questions.questions.forEach { question -> QuestionBlock(model, questions.requestId, question, disabled) }
        }
        RequestButton(
            "Submit answers",
            if (questions.submitEnabled) RequestTone.Primary else RequestTone.Secondary,
            enabled = !disabled && questions.submitEnabled,
            modifier = Modifier.fillMaxWidth(),
            large = true,
        ) {
            model.perform(Intent.SubmitAnswers(questions.requestId))
        }
        if (questions.dismissible)
            Box(
                Modifier.fillMaxWidth()
                    .clickable(enabled = !questions.responding) {
                        model.perform(Intent.DismissInput(questions.requestId))
                    }
                    .padding(vertical = 8.75.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    "Dismiss without answering",
                    style = AppTheme.footnote,
                    fontWeight = FontWeight.Bold,
                    color = colors.foregroundMuted,
                )
            }
    }
}

@Composable
private fun QuestionBlock(model: AndroidAppModel, requestId: String, question: QuestionView, disabled: Boolean) {
    val colors = AppTheme.colors
    Column(Modifier.padding(top = 3.5.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
        Text(
            question.header.uppercase(),
            style = AppTheme.caption,
            fontWeight = FontWeight.Bold,
            letterSpacing = 1.sp,
            color = colors.foregroundMuted,
        )
        Text(question.question, style = AppTheme.body, color = colors.foreground)
        question.options.forEach { option ->
            Surface(
                onClick = {
                    model.perform(Intent.EditAnswer(requestId, question.id, AnswerEdit.ToggleOption(option.value)))
                },
                enabled = !disabled,
                shape = RoundedCornerShape(16.dp),
                color = if (option.selected) colors.primary.copy(alpha = 0.1f) else colors.input,
                border = BorderStroke(1.dp, if (option.selected) colors.primary else colors.border),
                modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp),
            ) {
                Column(Modifier.padding(horizontal = 12.25.dp, vertical = 10.5.dp)) {
                    Text(
                        option.label,
                        style = AppTheme.footnote,
                        fontWeight = FontWeight.Bold,
                        color = if (option.selected) colors.foreground else colors.foregroundSecondary,
                    )
                    option.description
                        ?.takeIf { it != option.label }
                        ?.let { Text(it, style = AppTheme.footnote, color = colors.foregroundMuted) }
                }
            }
        }
        val draftKey = answerDraftKey(requestId, question.id)
        AttachmentButton(model, draftKey, enabled = !disabled, compact = true)
        DraftAttachmentStrip(model, model.snapshot.draftAttachments(draftKey), draftKey, thumbnail = 72.dp)
        var answer by remember(requestId, question.id) { mutableStateOf(question.customAnswer) }
        SettingsField(
            answer,
            {
                answer = it
                model.perform(Intent.EditAnswer(requestId, question.id, AnswerEdit.Custom(it)))
            },
            question.hint ?: "Type your answer",
        )
    }
}

package dev.remoteagent.mobile

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.QuestionAnswer
import dev.remoteagent.core.TimelineRow
import dev.remoteagent.core.questionAnswerValues
import dev.remoteagent.core.questionError
import dev.remoteagent.core.questionOptionSelected

@Composable
// Declarative native layout; the conversation decisions are supplied by core.
@Suppress("LongMethod", "CyclomaticComplexMethod")
internal fun RequestCard(model: AndroidAppModel, row: TimelineRow) {
    var selected by remember(row.id) { mutableStateOf(emptyMap<String, Set<String>>()) }
    var custom by remember(row.id) { mutableStateOf(emptyMap<String, String>()) }
    val answers =
        row.questions.map { question ->
            val text = custom[question.id].orEmpty()
            val values = questionAnswerValues(selected[question.id].orEmpty().sorted(), text, question.multiSelect)
            QuestionAnswer(question.id, values)
        }
    Surface(
        color = T3.color("mobileGroupedCard"),
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, T3.color("border")),
    ) {
        Column(Modifier.fillMaxWidth().padding(14.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(row.title, style = MaterialTheme.typography.titleSmall)
            if (row.text.isNotEmpty())
                androidx.compose.foundation.text.selection.SelectionContainer {
                    Text(row.text, style = MaterialTheme.typography.bodyMedium)
                }
            row.requestId?.let { request ->
                row.choices.forEach { choice ->
                    choice.warning?.let {
                        Text(it, color = T3.color("warningForeground"), style = MaterialTheme.typography.bodySmall)
                    }
                    OutlinedButton(
                        onClick = { model.perform(Intent.RespondApproval(request, choice.decision)) },
                        enabled = row.actionable,
                    ) {
                        Text(choice.label)
                    }
                }
                row.questions.forEach { question ->
                    Column(verticalArrangement = Arrangement.spacedBy(7.dp)) {
                        Text(question.header, style = MaterialTheme.typography.labelLarge)
                        Text(question.question, style = MaterialTheme.typography.bodyMedium)
                        question.options.forEach { option ->
                            OutlinedButton(
                                onClick = {
                                    val old = selected[question.id].orEmpty()
                                    selected =
                                        selected +
                                            (question.id to
                                                if (question.multiSelect) {
                                                    if (option.value in old) old - option.value else old + option.value
                                                } else setOf(option.value))
                                    if (!question.multiSelect) custom = custom - question.id
                                },
                                enabled = row.actionable,
                                modifier = Modifier.fillMaxWidth(),
                            ) {
                                Row(
                                    Modifier.fillMaxWidth(),
                                    verticalAlignment = Alignment.Top,
                                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                                ) {
                                    Text(
                                        if (
                                            questionOptionSelected(
                                                selected[question.id].orEmpty().toList(),
                                                custom[question.id].orEmpty(),
                                                question.multiSelect,
                                                option.value,
                                            )
                                        )
                                            "●"
                                        else "○"
                                    )
                                    Column {
                                        Text(option.label, style = MaterialTheme.typography.bodyMedium)
                                        if (option.description.isNotEmpty())
                                            Text(
                                                option.description,
                                                color = T3.color("textMuted"),
                                                style = MaterialTheme.typography.bodySmall,
                                            )
                                    }
                                }
                            }
                        }
                        if (question.allowCustomAnswer || question.options.isEmpty())
                            OutlinedTextField(
                                custom[question.id].orEmpty(),
                                { custom = custom + (question.id to it) },
                                Modifier.fillMaxWidth(),
                                enabled = row.actionable,
                                placeholder = { Text("Your answer") },
                            )
                    }
                }
                if (row.responseModeMessage) {
                    Text(
                        "Reply in the composer",
                        color = T3.color("textMuted"),
                        style = MaterialTheme.typography.bodySmall,
                    )
                    TextButton(onClick = { model.perform(Intent.DismissInput(request)) }, enabled = row.actionable) {
                        Text("Dismiss without answering")
                    }
                } else if (row.questions.isNotEmpty()) {
                    val error = questionError(row.questions, answers)
                    Button(
                        onClick = { model.perform(Intent.RespondQuestions(request, answers)) },
                        enabled = row.actionable && error == null,
                    ) {
                        Text("Submit answers")
                    }
                    error?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = T3.color("textMuted")) }
                }
            } ?: Text(row.status, style = MaterialTheme.typography.bodySmall, color = T3.color("textMuted"))
        }
    }
}

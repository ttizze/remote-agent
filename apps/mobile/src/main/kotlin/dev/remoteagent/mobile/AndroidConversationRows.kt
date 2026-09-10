package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.AgentException
import dev.remoteagent.core.Answer
import dev.remoteagent.core.Intent
import dev.remoteagent.core.JsonValue
import dev.remoteagent.core.Request
import dev.remoteagent.core.RequestKind
import dev.remoteagent.core.parseJsonValue

@Composable
internal fun RequestCard(request: Request, model: AndroidAppModel) {
    var busy by remember(request.key) { mutableStateOf(false) }
    var error by remember(request.key) { mutableStateOf<String?>(null) }
    fun respond(answer: Answer) {
        busy = true
        model.perform(Intent.Respond(request.id, answer)) {
            busy = false
            error = it.exceptionOrNull()?.message
        }
    }
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(request.title, style = MaterialTheme.typography.labelLarge)
            Text(request.body)
            when (request.kind) {
                RequestKind.COMMAND_APPROVAL,
                RequestKind.FILE_APPROVAL ->
                    request.decisionLabels.forEachIndexed { index, label ->
                        Button(onClick = { respond(Answer.Decision(index.toUInt())) }, enabled = !busy) {
                            Text(label)
                        }
                    }
                RequestKind.PERMISSIONS ->
                    Row {
                        Button(onClick = { respond(Answer.Permissions(true)) }, enabled = !busy) { Text("このターンで許可") }
                        Button(onClick = { respond(Answer.Permissions(false)) }, enabled = !busy) { Text("拒否") }
                    }
                RequestKind.QUESTIONS -> QuestionAnswers(request, busy, ::respond)
                else -> RawAnswer(busy, ::respond) { error = it }
            }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        }
    }
}

@Composable
private fun QuestionAnswers(request: Request, busy: Boolean, respond: (Answer) -> Unit) {
    var answers by remember(request.key) { mutableStateOf(emptyMap<String, String>()) }
    val questions = request.params["questions"]?.values.orEmpty()
    questions.forEach { question ->
        val id = question["id"]?.text.orEmpty()
        Text(question["question"]?.text ?: "回答")
        question["options"]?.values.orEmpty().forEach { option ->
            val label = option["label"]?.text.orEmpty()
            TextButton(onClick = { answers = answers + (id to label) }, enabled = !busy) { Text(label) }
        }
        OutlinedTextField(
            answers[id].orEmpty(),
            { answers = answers + (id to it) },
            enabled = !busy,
            visualTransformation =
                if ((question["isSecret"] as? JsonValue.Boolean)?.value == true) PasswordVisualTransformation()
                else VisualTransformation.None,
        )
    }
    Button(
        onClick = { respond(Answer.Questions(answers)) },
        enabled = !busy && questions.all { !answers[it["id"]?.text].isNullOrBlank() },
    ) {
        Text("回答を送信")
    }
}

@Composable
private fun RawAnswer(busy: Boolean, respond: (Answer) -> Unit, onError: (String?) -> Unit) {
    var raw by remember { mutableStateOf("{}") }
    OutlinedTextField(raw, { raw = it }, Modifier.fillMaxWidth(), enabled = !busy, label = { Text("応答 JSON") })
    Button(
        onClick = {
            try {
                respond(Answer.Raw(parseJsonValue(raw)))
            } catch (failure: AgentException) {
                onError(failure.message)
            }
        },
        enabled = !busy,
    ) {
        Text("応答を送信")
    }
}


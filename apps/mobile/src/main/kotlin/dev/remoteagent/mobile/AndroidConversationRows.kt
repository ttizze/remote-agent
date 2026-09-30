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
import dev.remoteagent.core.Choice
import dev.remoteagent.core.ElicitationAnswer
import dev.remoteagent.core.ElicitationInput
import dev.remoteagent.core.Question
import dev.remoteagent.core.Request
import dev.remoteagent.core.RequestBody
import dev.remoteagent.core.buildQuestionAnswer
import dev.remoteagent.core.requestAnswerFromJson
import dev.remoteagent.core.requestInputDefault

@Composable
internal fun RequestCard(request: Request, submit: (Answer, (String?) -> Unit) -> Unit) {
    var busy by remember(request.id) { mutableStateOf(false) }
    var error by remember(request.id) { mutableStateOf<String?>(null) }
    fun respond(answer: Answer) {
        busy = true
        submit(answer) {
            busy = false
            error = it
        }
    }
    val disabled = busy || !request.canRespond
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(request.title, style = MaterialTheme.typography.labelLarge)
            Text(request.body)
            if (request.details.isNotEmpty()) Text(request.details, style = MaterialTheme.typography.bodySmall)
            when (val body = request.requestBody) {
                is RequestBody.Approval -> ChoiceButtons(body.choices, false, disabled, ::respond)
                is RequestBody.Permission -> ChoiceButtons(body.choices, true, disabled, ::respond)
                is RequestBody.Question -> QuestionAnswers(request.id, body.questions, disabled, ::respond)
                is RequestBody.Elicitation -> {
                    ElicitationDescription(body.input, !disabled)
                    StructuredAnswer(request, disabled, ::respond) { error = it }
                    Row {
                        TextButton(
                            onClick = { respond(Answer.Elicitation(ElicitationAnswer.Decline)) },
                            enabled = !disabled,
                        ) {
                            Text("辞退")
                        }
                        TextButton(
                            onClick = { respond(Answer.Elicitation(ElicitationAnswer.Cancel)) },
                            enabled = !disabled,
                        ) {
                            Text("キャンセル")
                        }
                    }
                }
                is RequestBody.ToolExecution -> StructuredAnswer(request, disabled, ::respond) { error = it }
            }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        }
    }
}

@Composable
private fun ElicitationDescription(input: ElicitationInput, enabled: Boolean) {
    when (input) {
        is ElicitationInput.Url -> {
            val context = androidx.compose.ui.platform.LocalContext.current
            TextButton(
                onClick = {
                    context.startActivity(
                        android.content.Intent(android.content.Intent.ACTION_VIEW, android.net.Uri.parse(input.url))
                    )
                },
                enabled = enabled,
            ) {
                Text("リンクを開く")
            }
        }
        is ElicitationInput.Form ->
            input.fields.forEach { field ->
                Text("${field.title} (${field.name})${if (field.required) " *" else ""}")
                if (field.description.isNotEmpty()) Text(field.description)
            }
    }
}

@Composable
private fun ChoiceButtons(choices: List<Choice>, permission: Boolean, disabled: Boolean, respond: (Answer) -> Unit) {
    choices.forEach { choice ->
        Button(
            onClick = { respond(if (permission) Answer.Permission(choice.id) else Answer.Approval(choice.id)) },
            enabled = !disabled,
        ) {
            Text(choice.label)
        }
        if (choice.description.isNotEmpty()) Text(choice.description)
    }
}

@Composable
private fun QuestionAnswers(key: String, questions: List<Question>, disabled: Boolean, respond: (Answer) -> Unit) {
    var answers by remember(key) { mutableStateOf(emptyMap<String, String>()) }
    var selections by remember(key) { mutableStateOf(emptyMap<String, List<String>>()) }
    questions.forEach { question ->
        val id = question.id
        Text(question.prompt)
        question.choices.forEach { choice ->
            val selected = selections[id].orEmpty()
            TextButton(
                onClick = {
                    selections =
                        selections +
                            (id to
                                if (question.multiple) {
                                    if (choice.id in selected) selected - choice.id else selected + choice.id
                                } else listOf(choice.id))
                    answers = answers + (id to "")
                },
                enabled = !disabled,
            ) {
                Text("${if (choice.id in selected) "✓ " else ""}${choice.label}")
            }
            if (choice.description.isNotEmpty()) Text(choice.description)
        }
        if (question.allowFreeText)
            OutlinedTextField(
                answers[id].orEmpty(),
                { answers = answers + (id to it) },
                enabled = !disabled,
                visualTransformation =
                    if (question.secret) PasswordVisualTransformation() else VisualTransformation.None,
            )
    }
    Button(
        onClick = {
            respond(
                Answer.Questions(
                    questions.associate { question ->
                        question.id to
                            buildQuestionAnswer(
                                question.multiple,
                                answers[question.id].orEmpty(),
                                selections[question.id].orEmpty(),
                            )
                    }
                )
            )
        },
        enabled = !disabled,
    ) {
        Text("回答を送信")
    }
}

@Composable
private fun StructuredAnswer(
    request: Request,
    disabled: Boolean,
    respond: (Answer) -> Unit,
    onError: (String?) -> Unit,
) {
    var text by remember(request.id) { mutableStateOf(requestInputDefault(request.requestBody)) }
    val urlConfirmation = (request.requestBody as? RequestBody.Elicitation)?.input is ElicitationInput.Url
    if (!urlConfirmation) OutlinedTextField(text, { text = it }, Modifier.fillMaxWidth(), enabled = !disabled)
    Button(
        onClick = {
            try {
                respond(requestAnswerFromJson(request.requestBody, text))
            } catch (failure: AgentException) {
                onError(failure.message)
            }
        },
        enabled = !disabled,
    ) {
        Text(if (urlConfirmation) "確認して送信" else "回答を送信")
    }
}

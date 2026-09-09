package dev.remoteagent.mobile

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject

@Composable
internal fun ThreadMessageCard(item: CodexItem, isUser: Boolean) {
    val message = item.toThreadItemPresentation().collapsedBody
    if (isUser) {
        Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
            Card {
                Column(modifier = Modifier.padding(12.dp)) {
                    Text(message)
                    (item as? CodexItem.UserMessage)?.imageSources.orEmpty().forEach { source ->
                        Text(attachmentMessageLabel(true, source, ""))
                    }
                }
            }
        }
    } else {
        Text(message, modifier = Modifier.fillMaxWidth())
    }
}

@Composable
internal fun ThreadActivityCard(item: CodexItem, isExpanded: Boolean, toggleExpanded: () -> Unit) {
    val presentation = item.toThreadItemPresentation()
    Card(
        modifier =
            Modifier.fillMaxWidth()
                .then(if (presentation.isCollapsible) Modifier.clickable(onClick = toggleExpanded) else Modifier)
    ) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (presentation.isCollapsible) Text(if (isExpanded) "⌄" else "›")
                Text(presentation.title, style = MaterialTheme.typography.labelLarge, maxLines = 1)
            }
            val body = if (isExpanded) item.expandedThreadItemBody() else presentation.collapsedBody
            if (body.isNotEmpty()) {
                Text(text = body, maxLines = if (presentation.isCollapsible && !isExpanded) 1 else Int.MAX_VALUE)
            }
        }
    }
}

@Composable
internal fun ThreadRequestCard(
    request: ThreadRequestPresentation,
    params: JsonObject,
    onRespond: suspend (RequestAnswer) -> GatewayResult<Unit>,
) {
    var busy by remember(request.id) { mutableStateOf(false) }
    var resolved by remember(request.id) { mutableStateOf(false) }
    var error by remember(request.id) { mutableStateOf<String?>(null) }
    var detailsExpanded by remember(request.id) { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    if (!resolved) {
        Card(modifier = Modifier.fillMaxWidth()) {
            Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(request.title, style = MaterialTheme.typography.labelLarge)
                if (request.questions.isEmpty()) Text(request.body)
                TextButton(onClick = { detailsExpanded = !detailsExpanded }) { Text("詳細") }
                if (detailsExpanded) {
                    Text(remember(params) { params.toString() }, style = MaterialTheme.typography.bodySmall)
                }
                ThreadRequestControls(request, enabled = !busy) { answer ->
                    busy = true
                    error = null
                    scope.launch {
                        try {
                            when (val result = onRespond(answer)) {
                                is GatewayResult.Success -> resolved = true
                                is GatewayResult.Failure -> error = result.message
                            }
                        } finally {
                            busy = false
                        }
                    }
                }
                error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            }
        }
    }
}

@Composable
private fun ThreadRequestControls(
    request: ThreadRequestPresentation,
    enabled: Boolean,
    submit: (RequestAnswer) -> Unit,
) {
    var answers by remember(request.id) { mutableStateOf(emptyMap<String, String>()) }
    var rawResponse by remember(request.id) { mutableStateOf("{}") }
    when (request.form) {
        "decision" ->
            request.decisions.forEachIndexed { index, label ->
                Button(onClick = { submit(RequestAnswer.Decision(index)) }, enabled = enabled) { Text(label) }
            }
        "permissions" -> {
            Button(onClick = { submit(RequestAnswer.Permissions(true)) }, enabled = enabled) { Text("このターンで許可") }
            Button(onClick = { submit(RequestAnswer.Permissions(false)) }, enabled = enabled) { Text("拒否") }
        }
        "questions" -> {
            request.questions.forEach { question ->
                RequestQuestionField(question, answers[question.id].orEmpty(), enabled) { answer ->
                    answers = answers + (question.id to answer)
                }
            }
            Button(onClick = { submit(RequestAnswer.Answers(answers)) }, enabled = enabled) { Text("回答を送信") }
        }
        else -> {
            OutlinedTextField(
                value = rawResponse,
                onValueChange = { rawResponse = it },
                label = { Text("応答 JSON") },
                enabled = enabled,
                modifier = Modifier.fillMaxWidth(),
            )
            Button(onClick = { submit(RequestAnswer.Raw(rawResponse)) }, enabled = enabled) { Text("応答を送信") }
        }
    }
}

@Composable
private fun RequestQuestionField(
    question: RequestQuestion,
    answer: String,
    enabled: Boolean,
    onAnswer: (String) -> Unit,
) {
    val passwordTransformation = remember { PasswordVisualTransformation() }
    val keyboardOptions =
        remember(question.secret) {
            KeyboardOptions(keyboardType = if (question.secret) KeyboardType.Password else KeyboardType.Text)
        }
    Text(question.prompt)
    question.options.forEach { option -> Button(onClick = { onAnswer(option) }, enabled = enabled) { Text(option) } }
    OutlinedTextField(
        value = answer,
        onValueChange = onAnswer,
        label = { Text("回答") },
        enabled = enabled,
        modifier = Modifier.fillMaxWidth(),
        keyboardOptions = keyboardOptions,
        visualTransformation = if (question.secret) passwordTransformation else VisualTransformation.None,
    )
}

@Composable
internal fun ThreadErrorCard(error: ThreadErrorPresentation) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (error.isReconnecting) {
                    CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
                }
                Text(
                    error.title,
                    style = MaterialTheme.typography.labelLarge,
                    color =
                        if (error.isReconnecting) MaterialTheme.colorScheme.onSurface
                        else MaterialTheme.colorScheme.error,
                )
            }
            Text(error.message)
            error.details?.takeIf(String::isNotBlank)?.let { details ->
                Text(details, style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

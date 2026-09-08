package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

internal fun codexItem(value: JsonElement): CodexItem {
    val raw = value.asObjectOrNull() ?: JsonObject(mapOf("value" to value))
    val id = raw.string("id").orEmpty()
    return when (raw.string("type")) {
        "userMessage" ->
            CodexItem.UserMessage(id, raw.userMessageText(), raw.string("clientId"), raw.userMessageImages())
        "agentMessage" -> CodexItem.AgentMessage(id, raw.textLike(), agentMessagePhase(raw.string("phase")))
        "reasoning" -> CodexItem.Reasoning(id, raw.textLike())
        "commandExecution" ->
            CodexItem.CommandExecution(
                id = id,
                command = raw.string("command").orEmpty(),
                cwd = raw.string("cwd"),
                output = raw.string("aggregatedOutput").orEmpty(),
                status = codexCommandStatus(raw.string("status")),
                exitCode = raw.int("exitCode"),
            )
        "fileChange" ->
            CodexItem.FileChange(
                id = id,
                changes = raw.array("changes").orEmpty().mapNotNull(::codexFileChange),
                status = codexFileChangeStatus(raw.string("status")),
            )
        else -> CodexItem.Unknown(id = id, codexType = raw.string("type") ?: "unknown", raw = raw)
    }
}

private fun codexFileChange(value: JsonElement): FileUpdateChange? {
    val raw = value.asObjectOrNull() ?: return null
    val kind =
        when (raw.childObject("kind")?.string("type")) {
            "add" -> FileUpdateKind.Add
            "delete" -> FileUpdateKind.Delete
            else -> FileUpdateKind.Update
        }
    return FileUpdateChange(raw.string("path").orEmpty(), kind, raw.string("diff").orEmpty())
}

private fun agentMessagePhase(phase: String?): AgentMessagePhase? =
    when (phase) {
        "commentary" -> AgentMessagePhase.Commentary
        "final_answer" -> AgentMessagePhase.FinalAnswer
        else -> null
    }

private fun codexCommandStatus(status: String?): CommandExecutionStatus =
    when (status) {
        "inProgress" -> CommandExecutionStatus.InProgress
        "failed" -> CommandExecutionStatus.Failed
        "declined" -> CommandExecutionStatus.Declined
        else -> CommandExecutionStatus.Completed
    }

private fun codexFileChangeStatus(status: String?): FileChangeStatus =
    when (status) {
        "inProgress" -> FileChangeStatus.InProgress
        "failed" -> FileChangeStatus.Failed
        "declined" -> FileChangeStatus.Declined
        else -> FileChangeStatus.Completed
    }

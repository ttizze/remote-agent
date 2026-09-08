package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull

internal fun CodexServerRequest.toThreadRequestPresentation(): ThreadRequestPresentation {
    val kindAndTitle =
        when (method) {
            "item/commandExecution/requestApproval" -> "approval" to "コマンドの承認待ち"
            "item/fileChange/requestApproval" -> "approval" to "ファイル変更の承認待ち"
            "item/permissions/requestApproval" -> "approval" to "権限の承認待ち"
            "item/tool/requestUserInput" -> "userInput" to "回答待ち"
            "mcpServer/elicitation/request" -> "mcpInput" to "MCPからの入力待ち"
            "item/tool/call" -> "toolInput" to "ツールの入力待ち"
            else -> "request" to "Codexからの確認待ち"
        }
    val question = (params["questions"] as? JsonArray)?.firstOrNull()?.let { it as? JsonObject }?.text("question")
    val body = question ?: params.text("reason") ?: params.text("message") ?: params.text("prompt") ?: "操作を続けるには応答が必要です"
    return ThreadRequestPresentation(id, kindAndTitle.first, kindAndTitle.second, body)
}

internal fun CodexTurnError.toThreadErrorPresentation(status: TurnStatus): ThreadErrorPresentation {
    val kind =
        when (val info = codexErrorInfo) {
            is JsonPrimitive -> info.contentOrNull
            is JsonObject -> info.keys.firstOrNull()
            else -> null
        }
    val httpStatus = (codexErrorInfo as? JsonObject)?.values?.firstOrNull() as? JsonObject
    val overloaded = kind == "serverOverloaded" || httpStatus?.text("httpStatusCode") == "429"
    val reconnecting = willRetry
    val retryable = status == TurnStatus.Interrupted || kind.isRetryableErrorKind()
    return ThreadErrorPresentation(
        title =
            if (reconnecting) {
                if (overloaded) "サーバーが混み合っています。再接続しています" else "再接続しています"
            } else errorTitle(kind),
        message = message,
        details = additionalDetails,
        isReconnecting = reconnecting,
        isRetryable = retryable,
    )
}

private fun String?.isConnectionErrorKind(): Boolean =
    when (this) {
        "httpConnectionFailed",
        "responseStreamConnectionFailed",
        "responseStreamDisconnected",
        "responseTooManyFailedAttempts" -> true
        else -> false
    }

private fun String?.isRetryableErrorKind(): Boolean =
    this == "serverOverloaded" || this == "internalServerError" || isConnectionErrorKind()

private fun errorTitle(kind: String?): String =
    when (kind) {
        "contextWindowExceeded" -> "コンテキストの上限に達しました"
        "sessionBudgetExceeded" -> "セッションの上限に達しました"
        "usageLimitExceeded" -> "利用上限に達しました"
        "serverOverloaded" -> "サーバーが混み合っています"
        "cyberPolicy",
        "misalignmentPolicyViolation" -> "安全ポリシーにより停止しました"
        "internalServerError" -> "サーバーエラー"
        "unauthorized" -> "認証が必要です"
        "badRequest" -> "リクエストを処理できません"
        "threadRollbackFailed" -> "タスクを元に戻せませんでした"
        "sandboxError" -> "サンドボックスエラー"
        "activeTurnNotSteerable" -> "この作業中はメッセージを追加できません"
        else -> if (kind.isConnectionErrorKind()) "接続エラー" else "エラー"
    }

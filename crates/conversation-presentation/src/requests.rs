use std::borrow::Cow;

use serde::Serialize;
use serde_json::Value;

use crate::{array, text};

const DEFAULT_DECISIONS: [&str; 4] = ["accept", "acceptForSession", "decline", "cancel"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Question<'a> {
    pub id: &'a str,
    pub prompt: &'a str,
    pub options: Vec<&'a str>,
    pub secret: bool,
}

#[derive(Serialize)]
pub struct RequestPresentation<'a> {
    pub kind: &'static str,
    pub form: &'static str,
    pub title: &'static str,
    pub body: &'a str,
    pub questions: Vec<Question<'a>>,
    pub decisions: Vec<Cow<'a, str>>,
}

pub fn request_presentation<'a>(method: &str, params: &'a Value) -> RequestPresentation<'a> {
    let (kind, form, title) = match method {
        "item/commandExecution/requestApproval" => ("approval", "decision", "コマンドの承認待ち"),
        "item/fileChange/requestApproval" => ("approval", "decision", "ファイル変更の承認待ち"),
        "item/permissions/requestApproval" => ("approval", "permissions", "権限の承認待ち"),
        "item/tool/requestUserInput" => ("userInput", "questions", "回答待ち"),
        "mcpServer/elicitation/request" => ("mcpInput", "raw", "MCPからの入力待ち"),
        "item/tool/call" => ("toolInput", "raw", "ツールの入力待ち"),
        _ => ("request", "raw", "Codexからの確認待ち"),
    };
    let body = params["questions"][0]["question"]
        .as_str()
        .or_else(|| params["reason"].as_str())
        .or_else(|| params["message"].as_str())
        .or_else(|| params["prompt"].as_str())
        .unwrap_or("操作を続けるには応答が必要です");
    let questions = if form == "questions" {
        array(&params["questions"])
            .iter()
            .map(|question| Question {
                id: text(question, "id"),
                prompt: text(question, "question"),
                options: array(&question["options"])
                    .iter()
                    .map(|option| text(option, "label"))
                    .collect(),
                secret: question["isSecret"] == true,
            })
            .collect()
    } else {
        Vec::new()
    };
    let decisions = if form == "decision" {
        match params["availableDecisions"].as_array() {
            Some(values) => values
                .iter()
                .map(|value| match value.as_str() {
                    Some(value) => decision_label(value),
                    None => Cow::Owned(value.to_string()),
                })
                .collect(),
            None => DEFAULT_DECISIONS
                .iter()
                .map(|value| decision_label(value))
                .collect(),
        }
    } else {
        Vec::new()
    };
    RequestPresentation {
        kind,
        form,
        title,
        body,
        questions,
        decisions,
    }
}

fn decision_label(value: &str) -> Cow<'_, str> {
    Cow::Borrowed(match value {
        "accept" => "承認",
        "acceptForSession" => "このセッションで許可",
        "decline" => "拒否",
        "cancel" => "中止",
        value => value,
    })
}

pub fn decision_at(params: &Value, index: usize) -> Option<Cow<'_, Value>> {
    match params["availableDecisions"].as_array() {
        Some(values) => values.get(index).map(Cow::Borrowed),
        None => DEFAULT_DECISIONS
            .get(index)
            .map(|value| Cow::Owned(Value::from(*value))),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPresentation<'a> {
    pub title: &'static str,
    pub message: &'a str,
    pub details: Option<&'a str>,
    pub is_reconnecting: bool,
    pub is_retryable: bool,
}

pub fn error_presentation<'a>(error: &'a Value, status: &str) -> ErrorPresentation<'a> {
    let info = &error["codexErrorInfo"];
    let kind = info
        .as_str()
        .or_else(|| info.as_object()?.keys().next().map(String::as_str))
        .unwrap_or_default();
    let overloaded = kind == "serverOverloaded"
        || info
            .get(kind)
            .and_then(|value| value.get("httpStatusCode"))
            .is_some_and(|code| code == 429 || code == "429");
    let is_reconnecting = error["willRetry"] == true;
    let connection_error = matches!(
        kind,
        "httpConnectionFailed"
            | "responseStreamConnectionFailed"
            | "responseStreamDisconnected"
            | "responseTooManyFailedAttempts"
    );
    let title = if is_reconnecting {
        if overloaded {
            "サーバーが混み合っています。再接続しています"
        } else {
            "再接続しています"
        }
    } else {
        match kind {
            "contextWindowExceeded" => "コンテキストの上限に達しました",
            "sessionBudgetExceeded" => "セッションの上限に達しました",
            "usageLimitExceeded" => "利用上限に達しました",
            "serverOverloaded" => "サーバーが混み合っています",
            "cyberPolicy" | "misalignmentPolicyViolation" => "安全ポリシーにより停止しました",
            "internalServerError" => "サーバーエラー",
            "unauthorized" => "認証が必要です",
            "badRequest" => "リクエストを処理できません",
            "threadRollbackFailed" => "タスクを元に戻せませんでした",
            "sandboxError" => "サンドボックスエラー",
            "activeTurnNotSteerable" => "この作業中はメッセージを追加できません",
            _ if connection_error => "接続エラー",
            _ => "エラー",
        }
    };
    ErrorPresentation {
        title,
        message: text(error, "message"),
        details: error["additionalDetails"].as_str(),
        is_reconnecting,
        is_retryable: status == "interrupted"
            || connection_error
            || matches!(kind, "serverOverloaded" | "internalServerError"),
    }
}

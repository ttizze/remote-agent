//! Shared error, progress, and partial-history wording for every native client.
use super::TurnErrorPresentation;
use crate::models;

pub(super) fn turn_error(error: &models::ExecutionError) -> TurnErrorPresentation {
    use models::ErrorCategory::*;
    let retrying = error.retry.as_ref().is_some_and(|retry| retry.retrying);
    let title = if retrying {
        if error.retry.as_ref().is_some_and(|retry| retry.overloaded) {
            "サーバーが混み合っています。再接続しています"
        } else {
            "再接続しています"
        }
    } else {
        match error.category {
            ContextLimit => "コンテキストの上限に達しました",
            SessionLimit => "セッションの上限に達しました",
            UsageLimit => "利用上限に達しました",
            Overloaded => "サーバーが混み合っています",
            RateLimited => "リクエストの上限に達しました",
            Policy => "安全ポリシーにより停止しました",
            Internal => "サーバーエラー",
            Auth => "認証が必要です",
            InvalidInput => "リクエストを処理できません",
            Rollback => "タスクを元に戻せませんでした",
            Sandbox => "サンドボックスエラー",
            InputUnavailable => "この作業中はメッセージを追加できません",
            Network => "接続エラー",
            Other => "エラー",
        }
    };
    TurnErrorPresentation {
        title: title.into(),
        message: error.message.clone(),
        details: error.details.clone(),
        is_reconnecting: retrying,
    }
}

pub(super) fn progress_label(
    turn: &models::Turn,
    action: Option<&str>,
    now_seconds: f64,
) -> String {
    let started = turn.started_at.as_ref().copied().or_else(|| {
        turn.started_at_ms
            .map(|milliseconds| milliseconds as f64 / 1000.)
    });
    let elapsed = started
        .map(|started| format!("{}秒 ", (now_seconds - started).max(0.) as u64))
        .unwrap_or_default();
    match action.filter(|action| !action.is_empty()) {
        Some(action) => format!("{elapsed}作業中 · {action}"),
        None => format!("{elapsed}作業中…"),
    }
}

pub(super) fn history_notice(thread: &models::Thread) -> Option<String> {
    use crate::session::HistoryReadKind;
    let state = thread.history_read_state.as_ref()?;
    let heading = match state.kind {
        HistoryReadKind::Partial => "履歴の一部を表示しています。",
        HistoryReadKind::Incomplete => "履歴の一部を読み取れませんでした。",
        HistoryReadKind::Unavailable => {
            "履歴を取得できません。保存済みの表示は最新とは限りません。"
        }
        _ => return None,
    };
    let issues = state
        .issues
        .iter()
        .take(8)
        .map(|id| id.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Some(if issues.is_empty() {
        heading.into()
    } else {
        format!("{heading}\n{issues}")
    })
}

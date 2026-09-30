use serde_json::Value;

pub fn error_message(error: &str) -> String {
    let Some(raw) = error.strip_prefix("remote RPC error: ") else {
        return error.into();
    };
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return "接続先で操作に失敗しました。エラーの詳細を取得できませんでした。".into();
    };
    if value["code"] == "dictation_failed" {
        let reason = value["message"]
            .as_str()
            .filter(|message| !message.trim().is_empty())
            .map(crate::diagnostics::sanitize)
            .unwrap_or_else(|| "もう一度録音してください。".into());
        return format!("音声を文字起こしできませんでした。{reason}");
    }
    value["message"]
        .as_str()
        .filter(|message| !message.trim().is_empty())
        .map(crate::diagnostics::sanitize)
        .unwrap_or_else(|| {
            "接続先で操作に失敗しました。エラーの詳細を取得できませんでした。".into()
        })
}

#[cfg(test)]
mod error_tests {
    use super::error_message;
    #[test]
    fn provider_cause_survives_host_envelope_without_exposing_payloads() {
        let raw = r#"remote RPC error: {"delivery":"unknown","code":"provider_failed","message":"thread example already has an active writer","execution":{"providerCode":"-32600"}}"#;
        assert_eq!(
            error_message(raw),
            "thread example already has an active writer"
        );
        assert_eq!(crate::diagnostics::sanitize(raw), error_message(raw));
        for raw in [
            "remote RPC error: invalid",
            r#"remote RPC error: {"message":" "}"#,
        ] {
            let message = error_message(raw);
            assert!(message.contains("詳細を取得できませんでした"));
            assert!(!message.contains("もう一度"));
        }
    }

    #[test]
    fn dictation_errors_explain_recovery_without_rendering_rpc_payloads() {
        let message = error_message(
            r#"remote RPC error: {"code":"dictation_failed","message":"provider failed","data":{"audio":"private"}}"#,
        );
        assert!(message.contains("音声を文字起こしできませんでした。provider failed"));
        assert!(!message.contains("private") && !message.contains("{"));
        assert!(error_message(r#"remote RPC error: {"code":"dictation_failed","message":"Codexにログインしてください。"}"#).contains("Codexにログインしてください。"));
        assert_eq!(
            error_message(r#"remote RPC error: {"code":-1,"message":"permission denied"}"#),
            "permission denied"
        );
        assert_eq!(
            error_message("マイクへのアクセスを許可してください"),
            "マイクへのアクセスを許可してください"
        );
    }
}

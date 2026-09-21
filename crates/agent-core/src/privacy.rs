//! Shared disclosure and consent revision; native clients own device storage.

#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DataSharingNotice {
    pub revision: String,
    pub requires_consent: bool,
    pub summary: String,
    pub policy: String,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn data_sharing_notice(accepted_revision: String) -> DataSharingNotice {
    let revision = "2026-09-21";
    DataSharingNotice {
        revision: revision.into(),
        requires_consent: accepted_revision != revision,
        summary: "メッセージ・添付・プロジェクトの内容を接続先Hostへ送信し、選択したAI（Codex: OpenAI / Claude: Anthropic）で処理します。音声入力の録音はOpenAIへ送信します。共有Hostの管理者や利用者も内容を読めます。\n\nMessages, attachments and project content go to your paired Host and the selected AI provider (OpenAI for Codex; Anthropic for Claude). Dictation audio goes to OpenAI. Other users of a shared Host may also access your data.".into(),
        policy: include_str!("../../../docs/PRIVACY.md").into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_consent_to_the_current_notice_is_accepted() {
        let notice = data_sharing_notice(String::new());
        assert!(notice.requires_consent);
        assert!(data_sharing_notice("earlier-notice".into()).requires_consent);
        assert!(!data_sharing_notice(notice.revision).requires_consent);
    }
}

//! A feedback report is an explicit user operation, independent of conversation input.
use crate::session::SessionRef;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadFeedback {
    /// Client notice identity. A new report uses a new UUID; reconnect never resends it.
    pub id: String,
    pub storage_scope: String,
    pub thread_id: SessionRef,
    pub reason: Option<String>,
    pub include_logs: bool,
}

impl UploadFeedback {
    pub fn validate(&self) -> Result<(), &'static str> {
        uuid::Uuid::parse_str(&self.id).map_err(|_| "feedback id must be a UUID")?;
        if self.storage_scope.is_empty() {
            return Err("feedback requires a Host storage identity");
        }
        self.thread_id
            .validate()
            .map_err(|_| "invalid feedback session")?;
        if self
            .reason
            .as_ref()
            .is_some_and(|reason| reason.encode_utf16().take(8001).count() > 8000)
        {
            return Err("feedback reason exceeds 8000 UTF-16 units");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackReceipt {
    pub feedback_id: String,
}

pub(crate) fn validate_receipt(
    _: &UploadFeedback,
    receipt: &FeedbackReceipt,
) -> Result<(), &'static str> {
    if receipt.feedback_id.trim().is_empty() || receipt.feedback_id.len() > 512 {
        Err("invalid feedback receipt")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_identity_and_reason_are_bounded_before_sending() {
        let mut request = UploadFeedback {
            id: uuid::Uuid::nil().to_string(),
            storage_scope: "host".into(),
            thread_id: SessionRef::new(crate::session::ProviderKind::Codex, "session".into())
                .unwrap(),
            reason: Some("😀".repeat(4000)),
            include_logs: false,
        };
        request.validate().unwrap();
        request.reason.as_mut().unwrap().push('a');
        assert!(request.validate().is_err());
        request.reason = None;
        request.id.clear();
        assert!(request.validate().is_err());
        let receipt = FeedbackReceipt {
            feedback_id: "report-id".into(),
        };
        validate_receipt(&request, &receipt).unwrap();
        assert!(
            validate_receipt(
                &request,
                &FeedbackReceipt {
                    feedback_id: " ".into()
                }
            )
            .is_err()
        );
    }
}

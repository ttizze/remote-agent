use super::*;

#[rstest::rstest]
#[case("plain diagnostic", "plain diagnostic")]
#[case(
    r#"{"error":{"message":"nested diagnostic"},"trace":"keep me"}"#,
    "nested diagnostic"
)]
#[case(r#"{"error":{"message":"  "}}"#, r#"{"error":{"message":"  "}}"#)]
#[case(r#"{"error":{"message":false}}"#, r#"{"error":{"message":false}}"#)]
#[case(r#"{"error":null}"#, r#"{"error":null}"#)]
#[case(r#"{"message":"top-level"}"#, r#"{"message":"top-level"}"#)]
#[case("{broken", "{broken")]
fn provider_envelopes_keep_the_verbatim_diagnostic_and_only_extract_a_nonempty_message(
    #[case] raw: &str,
    #[case] summary: &str,
) {
    let banner = ThreadErrorBanner::from_failure(&ExecutionError {
        message: raw.into(),
        ..Default::default()
    });
    assert_eq!(banner.text, raw);
    assert_eq!(banner.collapsed_text, summary);
    assert!(!banner.warning);
    assert_eq!(banner.action, None);
}

#[rstest::rstest]
#[case(None, ErrorCategory::Other, false, None)]
#[case(None, ErrorCategory::UsageLimit, true, None)]
#[case(
    Some("subscription_sharing_usage_limit_exceeded"),
    ErrorCategory::Other,
    true,
    Some(ThreadErrorAction::ManageUsage)
)]
#[case(
    Some("subscription_sharing_workplace_individual_usage_limit_exceeded"),
    ErrorCategory::Other,
    true,
    Some(ThreadErrorAction::CopyAdminRequest)
)]
#[case(Some("unknown"), ErrorCategory::Other, false, None)]
fn only_recognized_provider_codes_offer_the_corresponding_usage_action(
    #[case] code: Option<&str>,
    #[case] category: ErrorCategory,
    #[case] warning: bool,
    #[case] action: Option<ThreadErrorAction>,
) {
    let banner = ThreadErrorBanner::from_failure(&ExecutionError {
        message: "failure".into(),
        provider_code: code.map(str::to_owned),
        category,
        ..Default::default()
    });
    assert_eq!(banner.warning, warning);
    assert_eq!(banner.action, action);
}

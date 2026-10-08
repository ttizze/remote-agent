use agent_protocol::vcs::SourceControlProviderKind;

/// The provider-neutral words shown beside Git actions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ChangeRequestTerminology {
    pub singular: String,
    pub short_label: String,
}

pub fn change_request_terminology(
    provider: Option<SourceControlProviderKind>,
) -> ChangeRequestTerminology {
    match provider {
        Some(SourceControlProviderKind::Github) | None => ChangeRequestTerminology {
            singular: "pull request".into(),
            short_label: "PR".into(),
        },
        Some(SourceControlProviderKind::Gitlab) => ChangeRequestTerminology {
            singular: "merge request".into(),
            short_label: "MR".into(),
        },
        Some(SourceControlProviderKind::Forgejo)
        | Some(SourceControlProviderKind::AzureDevops)
        | Some(SourceControlProviderKind::Bitbucket)
        | Some(SourceControlProviderKind::Unknown) => ChangeRequestTerminology {
            singular: "change request".into(),
            short_label: "CR".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_words_are_stable() {
        assert_eq!(change_request_terminology(None).short_label, "PR");
        assert_eq!(
            change_request_terminology(Some(SourceControlProviderKind::Gitlab)).singular,
            "merge request"
        );
    }
}

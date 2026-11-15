use super::*;

#[test]
fn catalog_candidates_keep_unicode_name_path_provider_order_and_stable_ties() {
    use ProviderKind::{Claude, Codex};
    use agent_protocol::composer::{
        Invocation,
        InvocationKind::{Plugin, Skill},
    };
    let candidate = |provider, kind, name: &str, path: &str, description: &str| ComposerCandidate {
        invocation: Invocation {
            provider,
            kind,
            name: name.into(),
            path: path.into(),
        },
        description: description.into(),
    };
    let candidates = vec![
        candidate(Codex, Skill, "Beta", "/b", "beta"),
        candidate(Claude, Skill, "alpha", "/a", "claude"),
        candidate(Codex, Skill, "ALPHA", "/z", "uppercase"),
        candidate(Codex, Skill, "alpha", "/a", "first alpha"),
        candidate(Codex, Skill, "alpha", "/a", "ignored adjacent duplicate"),
        candidate(Codex, Plugin, "alpha", "/a", "different kind"),
        candidate(Codex, Skill, "alpha", "/a", "kept separated duplicate"),
        candidate(Codex, Skill, "Alpha", "/a", "different original case"),
        candidate(Codex, Skill, "ístanbul", "/a", "accent"),
        candidate(Codex, Skill, "İ", "/a", "unicode uppercase"),
        candidate(Codex, Skill, "i\u{307}", "/a", "unicode lowercase"),
    ];
    let expected = [3, 5, 6, 7, 1, 2, 0, 9, 10, 8]
        .into_iter()
        .map(|index| candidates[index].clone())
        .collect::<Vec<_>>();
    assert_eq!(ordered_candidates(candidates), expected);
    assert_eq!(ordered_candidates(expected.clone()), expected);
    assert_eq!(ordered_candidates(Vec::new()), Vec::new());
    assert_eq!(
        ordered_candidates(vec![expected[0].clone()]),
        vec![expected[0].clone()]
    );
}

#[tokio::test]
async fn startup_errors_stay_ordered_and_account_errors_keep_their_provider() {
    for providers in [
        [
            ProviderKind::DeepSeek,
            ProviderKind::Claude,
            ProviderKind::Codex,
        ],
        [
            ProviderKind::Claude,
            ProviderKind::Codex,
            ProviderKind::DeepSeek,
        ],
        [
            ProviderKind::Codex,
            ProviderKind::DeepSeek,
            ProviderKind::Claude,
        ],
    ] {
        let (registry, _) = Registry::new(providers.map(|provider| {
            Backend::unavailable(provider, format!("{} startup failed", provider.key()))
        }));
        let expected = "codex startup failed\nclaude startup failed\ndeepseek startup failed";
        assert_eq!(
            registry
                .startup_errors()
                .map(|(_, error)| error.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            expected
        );
        assert_eq!(
            registry.accounts().await.errors,
            providers
                .map(|provider| (provider, format!("{} startup failed", provider.key())))
                .into()
        );
    }
}

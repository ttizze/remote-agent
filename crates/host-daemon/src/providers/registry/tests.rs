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

#[test]
fn storage_scope_has_a_fixed_encoding() {
    let areas = [
        (ProviderKind::Codex, "/fixtures/codex".into()),
        (ProviderKind::Claude, "/fixtures/claude".into()),
    ];
    assert_eq!(
        storage_scope(areas).unwrap(),
        "8170d6192a00957e1b3b4a3da16963137015f8d36f1efaa8b8b347090f78476b",
    );
}

proptest::proptest! {
    #[test]
    fn storage_scope_ignores_input_order_but_tracks_provider_paths(
        codex in "[a-zA-Z0-9/_ .あ界é-]{1,64}",
        claude in "[a-zA-Z0-9/_ .あ界é-]{1,64}",
    ) {
        let codex = std::path::PathBuf::from(codex);
        let claude = std::path::PathBuf::from(claude);
        let original = storage_scope([
            (ProviderKind::Codex, codex.clone()),
            (ProviderKind::Claude, claude.clone()),
        ]).unwrap();
        proptest::prop_assert_eq!(&original, &storage_scope([
            (ProviderKind::Claude, claude.clone()),
            (ProviderKind::Codex, codex.clone()),
        ]).unwrap());
        for areas in [
            [(ProviderKind::Codex, codex.join("different")), (ProviderKind::Claude, claude.clone())],
            [(ProviderKind::Codex, codex.clone()), (ProviderKind::Claude, claude.join("different"))],
        ] {
            proptest::prop_assert_ne!(&original, &storage_scope(areas).unwrap());
        }
        proptest::prop_assert_ne!(&original, &storage_scope([
            (ProviderKind::Codex, codex),
        ]).unwrap());
    }
}

#[test]
fn creating_native_storage_does_not_change_its_identity() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native").join("projects");
    let before = canonical_storage_path(&path);
    std::fs::create_dir_all(&path).unwrap();
    let scope = storage_scope([(ProviderKind::Codex, before)]).unwrap();
    assert_eq!(
        scope,
        storage_scope([(ProviderKind::Codex, canonical_storage_path(&path)),]).unwrap()
    );
    #[cfg(unix)]
    {
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(root.path(), &alias).unwrap();
        assert_eq!(
            scope,
            storage_scope([(
                ProviderKind::Codex,
                canonical_storage_path(&alias.join("native/projects"))
            ),])
            .unwrap()
        );
    }
}

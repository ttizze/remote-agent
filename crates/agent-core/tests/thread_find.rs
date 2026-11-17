//! Literal outputs of pinned shared/threadSearch.ts, independent of Rust's
//! regex engine and of the native search renderer.
use agent_core::presentation::conversation::find_matches;

#[test]
fn thread_find_folds_keyboard_apostrophes_and_sigma_without_expanding_unicode_positions() {
    for (text, query, expected) in [
        (
            "don't don’t don‘t",
            "don't",
            vec!["don't", "don’t", "don‘t"],
        ),
        ("Σςσ", "σ", vec!["Σ", "ς", "σ"]),
        ("İ I i", "i", vec!["I", "i"]),
        ("😀ẞ ß", "ß", vec!["ẞ", "ß"]),
        ("[.*] literal", "[.*]", vec!["[.*]"]),
        ("alpha", "\u{feff}alpha\u{3000}", vec!["alpha"]),
    ] {
        let matches: Vec<_> = find_matches(text, query)
            .into_iter()
            .map(|range| &text[range.start..range.end])
            .collect();
        assert_eq!(matches, expected, "text={text:?}, query={query:?}");
    }
}

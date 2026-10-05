use super::*;

#[test]
fn ignores_section_headers_split_inside_a_multibyte_character() {
    for header in ["[remoté]", "[remot€]", "[remoté \"origin\"]", "[ré]"] {
        let config = format!(
            "{header}\n\turl = git@github.com:wrong/repo.git\n[remote \"origin\"]\n\turl = git@github.com:owner/repo.git\n"
        );
        assert_eq!(
            parse_origin_url(&config).as_deref(),
            Some("git@github.com:owner/repo.git"),
            "{header}"
        );
    }
}

#[test]
fn reads_remote_sections_in_both_header_forms() {
    assert_eq!(
        parse_origin_url("[REMOTE.Upstream]\n\turl = a\n[remote \"origin\"]\n\turl = b\n")
            .as_deref(),
        Some("b")
    );
    assert_eq!(
        parse_origin_url("[remote.upstream] ; note\n\turl = a\n").as_deref(),
        Some("a")
    );
}

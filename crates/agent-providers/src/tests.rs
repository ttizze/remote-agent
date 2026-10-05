use crate::*;
#[test]
fn sdk_version_matches_the_reference_lock() {
    assert_eq!(CLAUDE_SDK_VERSION, "0.3.276");
}

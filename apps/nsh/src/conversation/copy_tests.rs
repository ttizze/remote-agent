use super::*;
#[test]
fn native_message_copy_actions_keep_the_original_body_and_role() {
    let source = "日本語 **message** 👋";
    for (kind, label) in [
        (Kind::User, "発言をコピー"),
        (Kind::Response, "回答をコピー"),
    ] {
        let row = Row::new("message".into(), kind, Shape::Message, source);
        assert_eq!(row.body.as_str(), source);
        let native = native::project_row(&row, false, "target", "/repo", None, true, None);
        assert!(
            native["buttons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|button| button["action"] == "copy" && button["title"] == label)
        );
        assert_eq!(
            native["document"]["role"],
            if kind == Kind::User {
                "user"
            } else {
                "assistant"
            }
        );
    }
}

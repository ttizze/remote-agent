use super::*;
#[test]
fn native_message_copy_actions_keep_the_original_body_and_role() {
    let source = "日本語 **message** 👋";
    for (kind, label) in [
        (Kind::User, "発言をコピー"),
        (Kind::Response, "回答をコピー"),
    ] {
        let mut row = Row::new("message".into(), kind, Shape::Message, source);
        row.actions = kind == Kind::Response;
        assert_eq!(row.body.as_str(), source);
        let native = native::project_row(&row, false, "target", "/repo", None, true, None);
        assert!(
            native["buttons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|button| button["action"] == "copy" && button["title"] == label)
        );
        let copy = native["buttons"]
            .as_array()
            .unwrap()
            .iter()
            .find(|button| button["action"] == "copy")
            .unwrap();
        assert_eq!(
            copy["iconOnly"], true,
            "copy stays an icon, with a semantic label for accessibility"
        );
        assert!(!copy["icon"].as_str().unwrap().is_empty());
        assert_eq!(
            native["document"]["role"],
            if kind == Kind::User {
                "user"
            } else {
                "assistant"
            }
        );
        if kind == Kind::Response {
            row.actions = false;
            let hidden = native::project_row(&row, false, "target", "/repo", None, true, None);
            assert!(
                hidden["buttons"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|button| button["action"] != "copy"),
                "only the response's designated meta row has its actions"
            );
        }
    }
}

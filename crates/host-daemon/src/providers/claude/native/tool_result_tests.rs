use super::*;

#[test]
fn command_results_replace_output_and_preserve_exit_codes_without_valid_new_evidence() {
    let body = ItemBody::CommandExecution {
        output_omitted: false,
        command: "printf hi".into(),
        cwd: Some("/work".into()),
        output: "old output".into(),
        exit_code: Some(7),
    };
    for (metadata, expected) in [
        (Value::Null, 7),
        (json!({"exitCode":null}), 7),
        (json!({"exitCode":false}), 7),
        (json!({"exitCode":1.5}), 7),
        (json!({"exitCode":i64::MAX}), 7),
        (json!({"exitCode":u64::MAX}), 7),
        (json!({"exitCode":i32::MIN}), i32::MIN),
        (json!({"exitCode":0}), 0),
        (json!({"exitCode":i32::MAX}), i32::MAX),
    ] {
        let ItemBody::CommandExecution {
            command,
            cwd,
            output,
            exit_code,
            ..
        } = tool_result_body(&body, &json!("updated"), &metadata)
        else {
            panic!("command changed kind")
        };
        assert_eq!(
            (command.as_str(), cwd.as_deref(), output.as_str(), exit_code),
            ("printf hi", Some("/work"), "updated", Some(expected))
        );
    }
    for (content, expected) in [
        (json!("literal\ntext"), "literal\ntext"),
        (
            json!([{"text":"first"},{"text":""},7,{"text":false},"ignored",{"text":"last"}]),
            "first\n\nlast",
        ),
        (json!([]), ""),
        (json!({"text":"not an array"}), ""),
        (Value::Null, ""),
    ] {
        let ItemBody::CommandExecution { output, .. } =
            tool_result_body(&body, &content, &Value::Null)
        else {
            unreachable!()
        };
        assert_eq!(output, expected);
    }
}

#[test]
fn file_results_only_apply_valid_patch_metadata_to_one_change() {
    let change = FileChange {
        path: "/file".into(),
        kind: FileChangeKind::Update {
            move_path: Some("/old".into()),
        },
        diff: Some("retained diff".into()),
        proposal: Some(FileProposal::Write {
            content: "retained proposal".into(),
        }),
    };
    let patch =
        json!([{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-old","+new"]}]);
    let body = ItemBody::FileChange {
        changes: vec![change.clone()],
        output: "old output".into(),
    };
    for (kind, expected) in [
        ("create", FileChangeKind::Add),
        ("update", FileChangeKind::Update { move_path: None }),
        ("other", change.kind.clone()),
    ] {
        let ItemBody::FileChange { changes, output } = tool_result_body(
            &body,
            &json!("new output"),
            &json!({"type":kind,"structuredPatch":patch}),
        ) else {
            unreachable!()
        };
        assert_eq!(output, "new output");
        assert_eq!(
            changes,
            vec![FileChange {
                path: "/file".into(),
                kind: expected,
                diff: Some("@@ -1,1 +1,1 @@\n-old\n+new\n".into()),
                proposal: change.proposal.clone()
            }]
        );
    }
    for metadata in [
        Value::Null,
        json!({"structuredPatch":[]}),
        json!({"structuredPatch":[{"oldStart":-1,"lines":["+new"]}]}),
    ] {
        let ItemBody::FileChange { changes, .. } = tool_result_body(&body, &Value::Null, &metadata)
        else {
            unreachable!()
        };
        assert_eq!(changes, vec![change.clone()]);
    }
    for changes in [
        vec![],
        vec![
            change.clone(),
            FileChange {
                path: "/other".into(),
                kind: FileChangeKind::Delete,
                diff: None,
                proposal: None,
            },
        ],
    ] {
        let body = ItemBody::FileChange {
            changes: changes.clone(),
            output: "old".into(),
        };
        assert_eq!(
            tool_result_body(
                &body,
                &json!("new"),
                &json!({"type":"create","structuredPatch":patch})
            ),
            ItemBody::FileChange {
                changes,
                output: "new".into()
            }
        );
    }
}

#[test]
fn tool_and_subagent_results_preserve_every_other_observation_including_null_results() {
    let tool = json!({"toolCall":{"kind":"mcp","tool":"search","server":"server","namespace":"namespace","resourceUri":"mcp://resource","pluginId":"plugin","arguments":{"query":"input"},"result":{"old":"result"},"error":{"saved":"error"},"content":[{"text":{"text":"saved block"}}],"success":false,"durationMs":123}});
    let agent = json!({"subagent":{"tool":"Agent","prompt":"inspect","model":"model","effort":"high","sender":{"provider":"claude","id":"parent"},"receivers":[{"provider":"claude","id":"child"}],"states":[{"session":{"provider":"claude","id":"child"},"status":"completed","message":"saved state"}],"agentId":"existing","result":{"old":"result"}}});
    for (key, value) in [("toolCall", tool), ("subagent", agent)] {
        let body: ItemBody = serde_json::from_value(value.clone()).unwrap();
        for content in [
            Value::Null,
            json!({"new":[1,"result"]}),
            json!(["new",{"nested":true}]),
        ] {
            for metadata in [
                Value::Null,
                json!({"agentId":7}),
                json!({"agentId":""}),
                json!({"agentId":"new-agent"}),
            ] {
                let result = tool_result_body(&body, &content, &metadata);
                let stored = match &result {
                    ItemBody::ToolCall { result, .. } | ItemBody::Subagent { result, .. } => result,
                    _ => unreachable!(),
                };
                assert_eq!(stored.as_ref(), Some(&content));
                let mut expected = value.clone();
                expected[key]["result"] = content.clone();
                if key == "subagent"
                    && let Some(id) = metadata["agentId"].as_str()
                {
                    expected[key]["agentId"] = json!(id);
                }
                assert_eq!(serde_json::to_value(result).unwrap(), expected);
            }
        }
    }
    let prose = ItemBody::AssistantText {
        text: "unchanged".into(),
        phase: AssistantPhase::Commentary,
    };
    assert_eq!(
        tool_result_body(&prose, &json!("ignored"), &json!({"exitCode":1})),
        prose
    );
}

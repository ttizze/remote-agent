//! Deterministic external Claude Code boundary. All conversation state and
//! tool side effects live in the explicitly supplied working directory.
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, Write},
    path::Path,
};

fn native(mut value: Value) {
    if !matches!(value["type"].as_str(), Some("assistant" | "user")) {
        return;
    }
    let Some(home) = std::env::var_os("CLAUDE_CONFIG_DIR") else {
        return;
    };
    let args: Vec<_> = std::env::args().collect();
    let session = args
        .windows(2)
        .find(|pair| pair[0] == "--session-id" || pair[0] == "--resume")
        .unwrap()[1]
        .clone();
    let directory = Path::new(&home)
        .join("projects")
        .join("fixture-native-project");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{session}.jsonl"));
    let previous: Option<Value> = fs::read_to_string(&path).ok().and_then(|text| {
        text.lines()
            .last()
            .and_then(|line| serde_json::from_str(line).ok())
    });
    value["parentUuid"] = previous
        .map(|value| value["uuid"].clone())
        .unwrap_or(Value::Null);
    value["uuid"] = value.get("uuid").cloned().unwrap_or_else(|| {
        format!(
            "fixture-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
        .into()
    });
    value["sessionId"] = session.into();
    value["cwd"] = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .to_string()
        .into();
    value["version"] = "2.1.266".into();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{value}").unwrap();
}

fn emit(value: Value) {
    native(value.clone());
    let mut out = io::stdout().lock();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}

fn block(session: &str, message: &str, index: usize, kind: &str, text: &str) {
    let (field, delta) = if kind == "thinking" {
        ("thinking", "thinking_delta")
    } else {
        ("text", "text_delta")
    };
    emit(
        json!({"type":"stream_event","session_id":session,"event":{"type":"content_block_start","index":index,"content_block":{"type":kind,field:""}}}),
    );
    emit(
        json!({"type":"stream_event","session_id":session,"event":{"type":"content_block_delta","index":index,"delta":{"type":delta,field:text}}}),
    );
    emit(
        json!({"type":"assistant","apiBlockIndex":index,"uuid":format!("envelope-{message}-{index}"),"session_id":session,"message":{"id":message,"role":"assistant","content":[{"type":kind,field:text}]}}),
    );
    emit(
        json!({"type":"stream_event","session_id":session,"event":{"type":"content_block_stop","index":index}}),
    );
}

fn reply(session: &str, count: usize, text: &str) {
    let message = format!("message-{count}");
    emit(
        json!({"type":"stream_event","session_id":session,"event":{"type":"message_start","message":{"id":message}}}),
    );
    block(session, &message, 0, "thinking", "Fixture reasoning");
    block(session, &message, 1, "text", text);
    emit(
        json!({"type":"result","subtype":"success","session_id":session,"is_error":false,"result":text}),
    );
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("auth") {
        auth(&args);
        return;
    }

    let option = |name: &str| {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].clone())
    };
    assert!(args.iter().any(|arg| arg == "-p"));
    assert_eq!(option("--input-format").as_deref(), Some("stream-json"));
    assert_eq!(option("--output-format").as_deref(), Some("stream-json"));
    assert_eq!(option("--permission-prompt-tool").as_deref(), Some("stdio"));
    assert!(
        !args
            .iter()
            .any(|arg| arg.contains("skip-permissions") || arg == "--bare")
    );
    let config: Value = fs::read("claude-fixture.json")
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
        .unwrap_or(json!({}));
    let session = option("--resume")
        .or_else(|| option("--session-id"))
        .unwrap_or_else(|| "catalog".into());
    if session != "catalog" {
        let mut trace = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("claude-auth-homes.jsonl")
            .unwrap();
        writeln!(trace, "{}", std::env::var("CLAUDE_CONFIG_DIR").unwrap()).unwrap();
    }
    let path = format!("claude-session-{session}.json");
    let mut inputs: Vec<Value> = if option("--resume").is_some() {
        serde_json::from_slice(
            &fs::read(&path).expect("resume must find the original session in its original cwd"),
        )
        .unwrap()
    } else {
        assert!(
            !Path::new(&path).exists(),
            "a new session cannot overwrite a previous session"
        );
        Vec::new()
    };
    assert!(args.iter().any(|arg| arg == "--replay-user-messages"));
    let mut waiting = None;
    for line in io::stdin().lock().lines() {
        let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
        match value["type"].as_str().unwrap() {
            "control_request" if value["request"]["subtype"] == "initialize" => {
                if config["initializeError"] == true {
                    emit(
                        json!({"type":"control_response","response":{"subtype":"error","request_id":value["request_id"],"error":"fixture initialization failed"}}),
                    );
                } else {
                    emit(
                        json!({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":{
                            "models":[{"value":"default","displayName":"Fixture Claude","supportedEffortLevels":["low","high"]},{"value":"haiku","displayName":"Fixture Haiku"}],
                            "account":if config["unauthenticated"] == true {json!({})} else {json!({"subscriptionType":"Claude Max"})}
                        }}}),
                    );
                }
            }
            "control_request" if value["request"]["subtype"] == "get_usage" => {
                assert_eq!(value["request"]["skip_behaviors"], true);
                emit(
                    json!({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":{"rate_limits":{"five_hour":{"utilization":72,"resets_at":"2033-05-18T03:33:20Z"},"seven_day":{"utilization":39,"resets_at":"2033-05-24T03:33:20Z"}}}}}),
                );
            }
            "user" => {
                assert_ne!(session, "catalog");
                assert_eq!(value["session_id"], session);
                assert!(
                    config["unauthenticated"] != true,
                    "unauthenticated input must never reach Claude"
                );
                assert_eq!(option("--model").as_deref(), Some("default"));
                if waiting.take() == Some("wait") {
                    // The previous input can finish before the queued input is consumed.
                    emit(
                        json!({"type":"result","session_id":session,"is_error":false,"result":"finished waiting"}),
                    );
                }
                emit(value.clone());
                let content = value["message"]["content"].clone();
                inputs.push(
                    json!({"content":content,"effort":option("--effort"),"pid":std::process::id()}),
                );
                fs::write(&path, serde_json::to_vec(&inputs).unwrap()).unwrap();
                emit(json!({"type":"system","subtype":"init","session_id":session}));
                let text = content
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if config["resultError"] == true {
                    emit(
                        json!({"type":"result","session_id":session,"is_error":true,"errors":["fixture inference failed"]}),
                    );
                } else if text == "wait" {
                    waiting = Some("wait");
                    emit(
                        json!({"type":"stream_event","session_id":session,"event":{"type":"message_start","message":{"id":"waiting"}}}),
                    );
                    block(&session, "waiting", 0, "text", "Waiting for interruption");
                } else if text == "permission" || text == "question" {
                    let tool = if text == "question" {
                        "AskUserQuestion"
                    } else {
                        "Bash"
                    };
                    let input = if text == "question" {
                        json!({"questions":[{"question":"Which color?","header":"Color","multiSelect":false,"options":[{"label":"Blue","description":"Use blue"},{"label":"Red","description":"Use red"}]}]})
                    } else {
                        json!({"command":"printf approved > approved.txt"})
                    };
                    emit(
                        json!({"type":"assistant","uuid":"tool-message","message":{"id":"tool-message","role":"assistant","content":[{"type":"tool_use","id":"tool-1","name":tool,"input":input}]}}),
                    );
                    emit(
                        json!({"type":"control_request","request_id":"permission-1","request":{"subtype":"can_use_tool","tool_name":tool,"tool_use_id":"tool-1","input":input}}),
                    );
                    waiting = Some(if text == "question" {
                        "question"
                    } else {
                        "permission"
                    });
                } else {
                    reply(
                        &session,
                        inputs.len(),
                        &format!("reply {}: {text}", inputs.len()),
                    );
                }
            }
            "control_response" => {
                assert_eq!(value["response"]["request_id"], "permission-1");
                let response = &value["response"]["response"];
                let allowed = response["behavior"] == "allow";
                let text = if waiting == Some("question") && allowed {
                    response["updatedInput"]["answers"]["Which color?"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                } else if allowed {
                    fs::write("approved.txt", "approved").unwrap();
                    "approved".into()
                } else {
                    "denied".into()
                };
                emit(
                    json!({"type":"user","uuid":"tool-result","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":text,"is_error":!allowed}]}}),
                );
                reply(&session, inputs.len(), &text);
                waiting = None;
            }
            "control_request" if value["request"]["subtype"] == "interrupt" => {
                emit(
                    json!({"type":"control_response","response":{"subtype":"success","request_id":value["request_id"],"response":{}}}),
                );
                emit(
                    json!({"type":"result","session_id":session,"is_error":false,"result":"interrupted"}),
                );
                waiting = None;
            }
            _ => panic!("unexpected fixture input type"),
        }
    }
}

fn auth(args: &[String]) {
    let home = std::path::PathBuf::from(
        std::env::var_os("CLAUDE_CONFIG_DIR").expect("isolated auth home"),
    );
    fs::create_dir_all(&home).unwrap();
    let file = home.join("fixture-auth.json");
    match args.get(1).map(String::as_str) {
        Some("status") => {
            let value = fs::read(&file)
                .ok()
                .map(|bytes| serde_json::from_slice::<Value>(&bytes).unwrap())
                .unwrap_or(json!({"loggedIn":false,"authMethod":"none"}));
            println!("{value}");
        }
        Some("login") => {
            assert!(args.iter().any(|arg| arg == "--claudeai"));
            println!(
                "If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?state=fixture-only"
            );
            print!("Paste code here if prompted > ");
            io::stdout().flush().unwrap();
            let mut code = String::new();
            io::stdin().read_line(&mut code).unwrap();
            if code.trim() != "fixture-code" {
                std::process::exit(1);
            }
            fs::write(file, serde_json::to_vec(&json!({"loggedIn":true,"authMethod":"claude.ai","email":"claude@example.invalid","subscriptionType":"max"})).unwrap()).unwrap();
        }
        Some("logout") => {
            let _ = fs::remove_file(file);
        }
        _ => panic!("unexpected auth command"),
    }
}

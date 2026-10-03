//! Deterministic external Claude Code boundary. Conversation history lives in
//! native storage; tool side effects and input traces use the working directory.
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, Write},
    path::Path,
};

fn native(mut value: Value) {
    if value["type"] == "system" && value["subtype"] == "task_notification" {
        let Some(tool) = value["tool_use_id"].as_str() else {
            return;
        };
        let prompt = format!(
            "<task-notification><task-id>{}</task-id><tool-use-id>{tool}</tool-use-id><status>{}</status><summary>{}</summary></task-notification>",
            value["task_id"].as_str().unwrap(),
            value["status"].as_str().unwrap(),
            value["summary"].as_str().unwrap()
        );
        value = json!({"type":"attachment","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":prompt}});
    }
    if !matches!(
        value["type"].as_str(),
        Some("assistant" | "user" | "attachment")
    ) {
        return;
    }
    let Some(home) = std::env::var_os("CLAUDE_CONFIG_DIR") else {
        return;
    };
    if let Some(result) = value.as_object_mut().unwrap().remove("tool_use_result") {
        value["toolUseResult"] = result;
    }
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

fn session_state(session: &str, state: &str) -> Value {
    json!({"type":"system","subtype":"session_state_changed","state":state,"session_id":session,"sdk_host_only":true})
}

fn emit(value: Value) {
    let idle = (value["type"] == "result")
        .then(|| session_state(value["session_id"].as_str().unwrap(), "idle"));
    native(value.clone());
    output(value);
    if let Some(idle) = idle {
        output(idle);
    }
}

fn output(value: Value) {
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

fn reply(session: &str, count: usize, text: &str, idle_before_result: bool) {
    let message = format!("message-{count}");
    emit(
        json!({"type":"stream_event","session_id":session,"event":{"type":"message_start","message":{"id":message}}}),
    );
    block(session, &message, 0, "thinking", "Fixture reasoning");
    block(session, &message, 1, "text", text);
    let result = json!({"type":"result","subtype":"success","session_id":session,"is_error":false,"result":text});
    if idle_before_result {
        output(session_state(session, "idle"));
        output(result);
    } else {
        emit(result);
    }
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
    assert_eq!(
        std::env::var("CLAUDE_CODE_SDK_READS_SESSION_STATE").as_deref(),
        Ok("1")
    );
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
        let config_home = std::env::var("CLAUDE_CONFIG_DIR").unwrap();
        let credentials_home = std::env::var("CLAUDE_SECURESTORAGE_CONFIG_DIR").unwrap();
        writeln!(trace, "{}", json!({
            "configHome": config_home,
            "credentialsHome": credentials_home,
            "skill": fs::read_to_string(Path::new(&config_home).join("skills/account-test/SKILL.md")).ok(),
            "settings": fs::read_to_string(Path::new(&config_home).join("settings.json")).ok(),
            "account": fs::read(Path::new(&credentials_home).join("fixture-auth.json")).ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|value| value["email"].as_str().map(str::to_owned)),
        })).unwrap();
    }
    let path = format!("claude-session-{session}.json");
    let history = Path::new(&std::env::var_os("CLAUDE_CONFIG_DIR").unwrap())
        .join("projects")
        .join("fixture-native-project")
        .join(format!("{session}.inputs.json"));
    let mut inputs: Vec<Value> = if option("--resume").is_some() {
        serde_json::from_slice(
            &fs::read(&history).expect("resume must find the original session in native storage"),
        )
        .unwrap()
    } else {
        assert!(
            !history.exists(),
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
                            "models":fs::read(Path::new(&std::env::var_os("CLAUDE_CONFIG_DIR").unwrap()).join("fixture-models.json"))
                                .ok()
                                .map(|bytes| serde_json::from_slice::<Value>(&bytes).unwrap())
                                .unwrap_or_else(|| json!([
                                {"value":"default","displayName":"Default (recommended)","description":"Opus 5 with 1M context · Best for everyday, complex tasks","supportedEffortLevels":["low","high"]},
                                {"value":"opus[1m]","displayName":"Opus (1M context)","description":"Opus 5 with 1M context · Best for everyday, complex tasks"},
                                {"value":"claude-fable-5-1[1m]","displayName":"Fable","description":"Fable 5.1 · Most capable for your hardest and longest-running tasks"},
                                {"value":"sonnet","displayName":"Sonnet","description":"Sonnet 5 · Efficient for routine tasks"},
                                {"value":"haiku","displayName":"Haiku","description":"Haiku 4.5 · Fastest for quick answers"},
                                {"value":"custom","displayName":"Custom model"}
                            ])),
                            "commands":[{"name":"fixture-skill","description":"Fixture skill"}],
                            "account":if config["unauthenticated"] == true {json!({})} else {json!({"subscriptionType":"Claude Max"})}
                        }}}),
                    );
                }
            }
            "control_request" if value["request"]["subtype"] == "get_usage" => {
                assert_eq!(value["request"]["skip_behaviors"], true);
                let home = std::env::var_os("CLAUDE_SECURESTORAGE_CONFIG_DIR").unwrap();
                let home = Path::new(&home);
                if home.join("usage-paused").exists() {
                    fs::write(home.join("usage-requested"), "").unwrap();
                    while home.join("usage-paused").exists() {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
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
                assert_eq!(
                    option("--model").as_deref(),
                    Some(config["expectedModel"].as_str().unwrap_or("default"))
                );
                if waiting.take() == Some("wait") {
                    // The previous input can finish before the queued input is consumed.
                    emit(
                        json!({"type":"result","session_id":session,"is_error":false,"result":"finished waiting"}),
                    );
                    native(json!({"type":"attachment","attachment":{
                        "type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"source_uuid":value["uuid"],
                        "prompt":value["message"]["content"]
                    }}));
                    output(value.clone());
                } else {
                    emit(value.clone());
                }
                let content = value["message"]["content"].clone();
                inputs.push(
                    json!({"content":content,"model":option("--model"),"effort":option("--effort"),"pid":std::process::id()}),
                );
                let bytes = serde_json::to_vec(&inputs).unwrap();
                fs::create_dir_all(history.parent().unwrap()).unwrap();
                fs::write(&history, &bytes).unwrap();
                fs::write(&path, bytes).unwrap();
                emit(json!({"type":"system","subtype":"init","session_id":session}));
                output(session_state(&session, "running"));
                let text = content
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if text == "crash" {
                    std::process::exit(17);
                } else if config["resultError"] == true {
                    emit(
                        json!({"type":"result","session_id":session,"is_error":true,"errors":["fixture inference failed"]}),
                    );
                } else if text == "wait" {
                    waiting = Some("wait");
                    let message = format!("waiting-{}", inputs.len());
                    emit(
                        json!({"type":"stream_event","session_id":session,"event":{"type":"message_start","message":{"id":message}}}),
                    );
                    block(&session, &message, 0, "text", "Waiting for interruption");
                } else if matches!(text.as_str(), "unknown_control" | "dialog" | "elicitation") {
                    let request = match text.as_str() {
                        "dialog" => json!({"subtype":"request_user_dialog","dialog_kind":"future"}),
                        "elicitation" => {
                            json!({"subtype":"elicitation","mcp_server_name":"fixture","message":"Name?","requested_schema":{"type":"object","required":["name"],"properties":{"name":{"type":"string","minLength":1}}}})
                        }
                        _ => json!({"subtype":"future_control"}),
                    };
                    emit(
                        json!({"type":"control_request","request_id":"control-1","request":request}),
                    );
                    waiting = Some(match text.as_str() {
                        "dialog" => "dialog",
                        "elicitation" => "elicitation",
                        _ => "unknown_control",
                    });
                } else if matches!(
                    text.as_str(),
                    "permission" | "question" | "background" | "background-settled"
                ) {
                    if text.starts_with("background") {
                        emit(
                            json!({"type":"system","subtype":"task_started","task_id":"background-1","task_type":"local_agent","description":"background work","session_id":session}),
                        );
                        let message = format!("background-{}", inputs.len());
                        emit(
                            json!({"type":"stream_event","session_id":session,"event":{"type":"message_start","message":{"id":message}}}),
                        );
                        block(&session, &message, 0, "text", "あとで報告します");
                        if text == "background-settled" {
                            emit(
                                json!({"type":"system","subtype":"task_notification","task_id":"background-1","status":"completed","summary":"background done","session_id":session}),
                            );
                        }
                        // The parent still owes a follow-up after this result.
                        output(
                            json!({"type":"result","session_id":session,"is_error":false,"result":"あとで報告します"}),
                        );
                    }
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
                        json!({"type":"assistant","uuid":format!("tool-message-{}", inputs.len()),"message":{"id":"tool-message","role":"assistant","content":[{"type":"tool_use","id":"tool-1","name":tool,"input":input}]}}),
                    );
                    emit(
                        json!({"type":"control_request","request_id":"permission-1","request":{"subtype":"can_use_tool","tool_name":tool,"tool_use_id":"tool-1","input":input}}),
                    );
                    waiting = Some(match text.as_str() {
                        "background-settled" => "background-settled",
                        "background" => "background",
                        "question" => "question",
                        _ => "permission",
                    });
                } else {
                    reply(
                        &session,
                        inputs.len(),
                        &format!("reply {}: {text}", inputs.len()),
                        config["idleBeforeResult"] == true,
                    );
                }
            }
            "control_response" => {
                if matches!(waiting, Some("unknown_control" | "dialog" | "elicitation")) {
                    assert_eq!(value["response"]["request_id"], "control-1");
                    match waiting.unwrap() {
                        "unknown_control" => assert_eq!(value["response"]["subtype"], "error"),
                        "dialog" => {
                            assert_eq!(value["response"]["response"]["behavior"], "cancelled")
                        }
                        "elicitation" => assert_eq!(
                            value["response"]["response"],
                            json!({"action":"accept","content":{"name":"BEX"}})
                        ),
                        _ => unreachable!(),
                    }
                    reply(
                        &session,
                        inputs.len(),
                        "control resolved",
                        config["idleBeforeResult"] == true,
                    );
                    waiting = None;
                    continue;
                }
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
                    json!({"type":"user","uuid":format!("tool-result-{}", inputs.len()),
                        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":text,"is_error":!allowed}]},
                        "tool_use_result":if allowed && waiting == Some("background") { json!({"backgroundTaskId":"background-1"}) }
                            else if allowed && waiting != Some("question") { json!({"exitCode":0}) } else { json!({}) }}),
                );
                if waiting == Some("background") {
                    emit(
                        json!({"type":"system","subtype":"task_notification","task_id":"background-1","tool_use_id":"tool-1","status":"completed","summary":"background done","session_id":session}),
                    );
                }
                reply(
                    &session,
                    inputs.len(),
                    &text,
                    config["idleBeforeResult"] == true,
                );
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

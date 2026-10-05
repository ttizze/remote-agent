//! CLI control protocol from claude-agent-sdk 0.3.276 (locked by T3 4ee6bfd).
use crate::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const CLAUDE_SDK_VERSION: &str = "0.3.276";
pub const CLAUDE_SDK_INTEGRITY: &str = "sha512-Dic43v4uuGLhibPAArWy3RSfq3zHdn+4Oh2AI2/18a5tlbNKkvChEpazyp8c+grUJvzxkzt0jETCa1wrADbkyw==";
#[derive(Debug, Default)]
pub struct ClaudeControl {
    next_request: u64,
    pending: BTreeMap<String, String>,
    permissions: BTreeMap<String, Value>,
}
impl ClaudeControl {
    pub fn request(&mut self, subtype: &str, payload: Value) -> Value {
        self.next_request += 1;
        let id = format!("request_{}", self.next_request);
        self.pending.insert(id.clone(), subtype.into());
        let mut request = payload;
        request["subtype"] = json!(subtype);
        json!({"type":"control_request","request_id":id,"request":request})
    }
    pub fn initialize(&mut self) -> Value {
        self.request("initialize", json!({"hooks":{},"sdkMcpServers":[]}))
    }
    pub fn receive(&mut self, frame: &Value) -> Result<Option<Translation>, ProtocolError> {
        match string(frame, "type").as_str() {
            "control_response" => {
                let response = &frame["response"];
                let id = required(response, "request_id")?;
                if let Some(operation) = self.pending.remove(&id)
                    && response["subtype"] == "error"
                {
                    return Err(ProtocolError::Remote {
                        operation,
                        message: string(response, "error"),
                        turn_completed: false,
                    });
                }
                Ok(Some(Translation::default()))
            }
            "control_cancel_request" => {
                let key = required(frame, "request_id")?;
                self.permissions.remove(&key);
                Ok(Some(Translation {
                    events: vec![ProviderEvent::RequestClosed { key }],
                    outbound: vec![],
                }))
            }
            "control_request" => {
                let key = required(frame, "request_id")?;
                let request = &frame["request"];
                match string(request, "subtype").as_str() {
                    "can_use_tool" => {
                        let tool = required(request, "tool_name")?;
                        let input = &request["input"];
                        if tool == "ExitPlanMode" {
                            let markdown = string(input, "plan");
                            let mut events = vec![];
                            if !markdown.trim().is_empty() {
                                events.push(ProviderEvent::Plan {
                                    key: optional(request, "tool_use_id")
                                        .unwrap_or_else(|| key.clone()),
                                    markdown,
                                    steps: vec![],
                                });
                            }
                            return Ok(Some(Translation {
                                events,
                                outbound: vec![control_success(
                                    &key,
                                    json!({"behavior":"deny","message":"Plan received. Wait for the user's next instruction.","toolUseID":request["tool_use_id"]}),
                                )],
                            }));
                        }
                        self.permissions.insert(key.clone(), request.clone());
                        let body = if tool == "AskUserQuestion" {
                            RequestBody::Questions {
                                questions: questions(&input["questions"]),
                            }
                        } else {
                            RequestBody::Approval {
                                kind: claude_request_kind(&tool).into(),
                                title: tool,
                                detail: optional(request, "description"),
                                options: vec![
                                    ApprovalOption {
                                        label: "Allow once".into(),
                                        decision: ApprovalDecision::Accept,
                                    },
                                    ApprovalOption {
                                        label: "Decline".into(),
                                        decision: ApprovalDecision::Decline,
                                    },
                                ],
                                input: Json(input.clone()),
                            }
                        };
                        Ok(Some(Translation {
                            events: vec![ProviderEvent::RequestOpened {
                                key,
                                body,
                                capability: ResponseCapability::Live,
                            }],
                            outbound: vec![],
                        }))
                    }
                    // SDK user dialogs are structured questions, rather than tool approvals.
                    "user_dialog" => {
                        self.permissions.insert(key.clone(), request.clone());
                        Ok(Some(Translation {
                            events: vec![ProviderEvent::RequestOpened {
                                key,
                                body: RequestBody::Questions {
                                    questions: questions(&request["questions"]),
                                },
                                capability: ResponseCapability::Live,
                            }],
                            outbound: vec![],
                        }))
                    }
                    _ => Ok(Some(Translation {
                        events: vec![],
                        outbound: vec![
                            json!({"type":"control_response","response":{"subtype":"error","request_id":key,"error":"Unsupported control request"}}),
                        ],
                    })),
                }
            }
            "keep_alive" => Ok(Some(Translation::default())),
            _ => Ok(None),
        }
    }
    pub fn respond(
        &mut self,
        key: &str,
        decision: Option<ApprovalDecision>,
        answers: Option<&Answers>,
    ) -> Result<Value, ProtocolError> {
        let request = self
            .permissions
            .remove(key)
            .ok_or_else(|| ProtocolError::Invalid("native request is no longer pending".into()))?;
        let tool = string(&request, "tool_name");
        let response = if let Some(answers) = answers {
            let mut input = request["input"].clone();
            // SDK AskUserQuestion expects each selected set joined into one string.
            input["answers"] = json!(
                answers
                    .iter()
                    .map(|(q, a)| (q.clone(), a.join(", ")))
                    .collect::<BTreeMap<_, _>>()
            );
            json!({"behavior":"allow","updatedInput":input,"toolUseID":request["tool_use_id"]})
        } else {
            permission_result(
                &tool,
                decision.unwrap_or(ApprovalDecision::Cancel),
                &request["input"],
                &request["tool_use_id"],
                request.get("permission_suggestions"),
            )
        };
        Ok(control_success(key, response))
    }
}
fn control_success(key: &str, response: Value) -> Value {
    json!({"type":"control_response","response":{"subtype":"success","request_id":key,"response":response}})
}
pub fn permission_result(
    tool: &str,
    decision: ApprovalDecision,
    input: &Value,
    tool_use_id: &Value,
    suggestions: Option<&Value>,
) -> Value {
    if matches!(
        decision,
        ApprovalDecision::Accept | ApprovalDecision::AcceptForSession
    ) {
        let mut result = json!({"behavior":"allow","updatedInput":input,"toolUseID":tool_use_id,"decisionClassification":if decision==ApprovalDecision::AcceptForSession { "user_permanent" } else { "user_temporary" }});
        if decision == ApprovalDecision::AcceptForSession {
            let mut updates = suggestions
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if updates.is_empty() {
                updates.push(json!({"type":"addRules","rules":[{"toolName":tool}],"behavior":"allow","destination":"session"}));
            } else {
                for update in &mut updates {
                    update["destination"] = json!("session");
                }
            }
            result["updatedPermissions"] = json!(updates);
        }
        result
    } else {
        let mut result = json!({"behavior":"deny","message":if decision==ApprovalDecision::Cancel { "User cancelled tool execution." } else { "User declined tool execution." },"toolUseID":tool_use_id,"decisionClassification":"user_reject"});
        if decision == ApprovalDecision::Cancel {
            result["interrupt"] = json!(true);
        }
        result
    }
}
pub fn claude_request_kind(tool: &str) -> &'static str {
    match tool.to_ascii_lowercase().as_str() {
        "edit" | "write" | "multiedit" | "notebookedit" => "file-change",
        "read" | "grep" | "glob" | "ls" => "file-read",
        _ => "command",
    }
}
pub fn claude_permission_mode(runtime: RuntimeMode, interaction: InteractionMode) -> &'static str {
    if interaction == InteractionMode::Plan {
        return "plan";
    }
    match runtime {
        RuntimeMode::ApprovalRequired => "default",
        RuntimeMode::AutoAcceptEdits => "acceptEdits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "bypassPermissions",
    }
}
#[derive(Debug, Clone)]
pub struct ClaudeLaunch {
    pub model: String,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub native_session: Option<String>,
    pub new_session: Option<String>,
    pub resume_at: Option<String>,
    pub fork: bool,
    pub additional_directories: Vec<String>,
    pub effort: Option<String>,
}
impl ClaudeLaunch {
    pub fn args(&self) -> Vec<String> {
        let mut args = [
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--thinking",
            "adaptive",
            "--thinking-display",
            "summarized",
            "--model",
            &self.model,
            "--permission-mode",
            claude_permission_mode(self.runtime_mode, self.interaction_mode),
        ]
        .map(str::to_owned)
        .to_vec();
        if self.runtime_mode == RuntimeMode::FullAccess {
            args.push("--allow-dangerously-skip-permissions".into());
        }
        if let Some(session) = &self.native_session {
            args.push(format!("--resume={session}"));
        } else if let Some(session) = &self.new_session {
            args.push(format!("--session-id={session}"));
        }
        if let Some(head) = &self.resume_at {
            args.push(format!("--resume-session-at={head}"));
        }
        if self.fork {
            args.push("--fork-session".into());
        }
        for directory in &self.additional_directories {
            args.extend(["--add-dir".into(), directory.clone()]);
        }
        if let Some(effort) = &self.effort {
            args.extend(["--effort".into(), effort.clone()]);
        }
        args
    }
}

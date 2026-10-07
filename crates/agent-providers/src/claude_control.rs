//! Host–SDK worker requests and application permission presentations.
use crate::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const CLAUDE_SDK_VERSION: &str = "0.3.276";
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
    pub fn initialize(&mut self, append_system_prompt: &str) -> Value {
        let mut payload = json!({});
        if !append_system_prompt.is_empty() {
            payload["appendSystemPrompt"] = json!(append_system_prompt);
        }
        self.request("initialize", payload)
    }
    pub fn receive(&mut self, frame: &Value) -> Result<Option<Translation>, ProtocolError> {
        match string(frame, "type").as_str() {
            "control_response" => {
                let response = &frame["response"];
                let id = required(response, "request_id")?;
                let Some(operation) = self.pending.remove(&id) else {
                    return Ok(Some(Translation::default()));
                };
                if response["subtype"] == "error" {
                    return Err(ProtocolError::Remote {
                        request: Some(id),
                        operation,
                        message: string(response, "error"),
                        turn_completed: false,
                    });
                }
                let output = Translation {
                    replies: vec![NativeReply {
                        request: id,
                        operation: operation.clone(),
                        result: Json(response["response"].clone()),
                    }],
                    ..Translation::default()
                };
                Ok(Some(output))
            }
            "control_cancel_request" => {
                let key = required(frame, "request_id")?;
                self.permissions.remove(&key);
                Ok(Some(Translation {
                    events: vec![ProviderEvent::RequestClosed { key }],
                    outbound: vec![],
                    ..Translation::default()
                }))
            }
            "control_request" => {
                let key = required(frame, "request_id")?;
                let request = &frame["request"];
                if self.permissions.contains_key(&key) {
                    return Ok(Some(Translation::default()));
                }
                match string(request, "subtype").as_str() {
                    "can_use_tool" => {
                        let tool = required(request, "tool_name")?;
                        let input = &request["input"];
                        if tool == "ExitPlanMode" {
                            let markdown = string(input, "plan");
                            let mut events = vec![];
                            if !markdown.trim().is_empty() {
                                events.push(ProviderEvent::Plan {
                                    kind: PlanKind::Proposed,
                                    key: optional(request, "tool_use_id")
                                        .unwrap_or_else(|| key.clone()),
                                    markdown: markdown.trim().into(),
                                    steps: vec![],
                                });
                            }
                            return Ok(Some(Translation {
                                events,
                                outbound: vec![control_success(
                                    &key,
                                    json!({"behavior":"deny","message":"The client captured your proposed plan. Stop here and wait for the user's feedback or implementation request in a later turn.","toolUseID":request["tool_use_id"]}),
                                )],
                                ..Translation::default()
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
                                        label: "Allow for session".into(),
                                        decision: ApprovalDecision::AcceptForSession,
                                    },
                                    ApprovalOption {
                                        label: "Cancel".into(),
                                        decision: ApprovalDecision::Cancel,
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
                                owner_path: vec![],
                                key,
                                body,
                                capability: ResponseCapability::Live,
                            }],
                            outbound: vec![],
                            ..Translation::default()
                        }))
                    }
                    // SDK user dialogs are structured questions, rather than tool approvals.
                    "request_user_dialog" if request["dialog_kind"] == "resume_return" => {
                        self.permissions.insert(key.clone(), request.clone());
                        let age = request["payload"]["sessionAgeMinutes"]
                            .as_f64()
                            .filter(|n| n.is_finite())
                            .unwrap_or(0.)
                            .max(0.)
                            .floor() as u64;
                        let tokens = request["payload"]["estimatedTokens"]
                            .as_f64()
                            .filter(|n| n.is_finite())
                            .unwrap_or(0.)
                            .max(0.)
                            .floor() as u64;
                        let age = if age >= 60 {
                            format!("{}h {}m", age / 60, age % 60)
                        } else {
                            format!("{age}m")
                        };
                        let digits = tokens.to_string();
                        let tokens = digits
                            .chars()
                            .enumerate()
                            .flat_map(|(i, c)| {
                                if i > 0 && (digits.len() - i).is_multiple_of(3) {
                                    vec![',', c]
                                } else {
                                    vec![c]
                                }
                            })
                            .collect::<String>();
                        let question = format!(
                            "This session is {age} old and uses {tokens} tokens. Compact it before continuing?"
                        );
                        Ok(Some(Translation {
                            events: vec![ProviderEvent::RequestOpened {
                                owner_path: vec![],
                                key,
                                body: RequestBody::Questions {
                                    questions: vec![Question { required:true,id:question.clone(),header:"Resume session".into(),question,multiple:false,options:vec![QuestionOption { label:"Compact and continue".into(),description:Some("Resume with a summary and use fewer tokens.".into()) },QuestionOption { label:"Keep full history".into(),description:Some("Resume without changing the conversation.".into()) },QuestionOption { label:"Don't ask again".into(),description:Some("Keep full history and skip future resume prompts.".into()) }] },],
                                },
                                capability: ResponseCapability::Live,
                            }],
                            outbound: vec![],
                            ..Translation::default()
                        }))
                    }
                    "request_user_dialog" => Ok(Some(Translation::default())),
                    _ => Ok(Some(Translation {
                        events: vec![],
                        outbound: vec![
                            json!({"type":"control_response","response":{"subtype":"error","request_id":key,"error":"Unsupported control request"}}),
                        ],
                        ..Translation::default()
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
        if request["subtype"] == "request_user_dialog" {
            let response = if let Some(answers) = answers {
                let answer = answers.values().next().map(Answer::text);
                json!({"behavior":"completed","result":match answer.as_deref() {Some("Compact and continue")=>"compact",Some("Don't ask again")=>"never",_=>"continue"}})
            } else {
                json!({"behavior":"cancelled"})
            };
            return Ok(control_success(key, response));
        }
        let response = if let Some(answers) = answers {
            let mut input = request["input"].clone();
            // SDK AskUserQuestion expects each selected set joined into one string.
            input["answers"] = json!(
                answers
                    .iter()
                    .map(|(q, a)| (q.clone(), a.text()))
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
/// The app's wait tools block for up to an hour, so the budget sits just above it.
pub const CLAUDE_MCP_TOOL_TIMEOUT_MS: u64 = 65 * 60 * 1_000;
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeRuntimeQueryPolicy {
    pub permission_mode: String,
    pub tools: Option<Vec<String>>,
    pub allowed_tools: Option<Vec<String>>,
    pub allow_dangerously_skip_permissions: bool,
    pub install_permission_callback: bool,
}
pub fn claude_runtime_query_policy(
    runtime: RuntimeMode,
    interaction: InteractionMode,
    approval_policy: Option<&str>,
    sandbox_kind: Option<&str>,
    read_only_allows_global_reads: bool,
) -> ClaudeRuntimeQueryPolicy {
    let mode = if interaction == InteractionMode::Plan {
        "plan"
    } else if approval_policy == Some("never") {
        match sandbox_kind {
            Some("readOnly") => "dontAsk",
            Some("dangerFullAccess" | "externalSandbox") => "bypassPermissions",
            _ => match runtime {
                RuntimeMode::ApprovalRequired => "dontAsk",
                RuntimeMode::AutoAcceptEdits => "acceptEdits",
                _ => "bypassPermissions",
            },
        }
    } else if approval_policy.is_some() {
        "default"
    } else {
        match sandbox_kind {
            Some("readOnly") => "dontAsk",
            Some("dangerFullAccess") if runtime != RuntimeMode::ApprovalRequired => {
                "bypassPermissions"
            }
            _ => claude_permission_mode(runtime, interaction),
        }
    };
    let tools = (sandbox_kind == Some("readOnly"))
        .then(|| ["Read", "Glob", "Grep"].map(str::to_owned).to_vec());
    ClaudeRuntimeQueryPolicy {
        permission_mode: mode.into(),
        allowed_tools: tools.clone().filter(|_| read_only_allows_global_reads),
        tools,
        allow_dangerously_skip_permissions: mode == "bypassPermissions",
        install_permission_callback: approval_policy.map_or_else(
            || {
                matches!(
                    runtime,
                    RuntimeMode::ApprovalRequired | RuntimeMode::AutoAcceptEdits
                )
            },
            |policy| policy != "never",
        ),
    }
}
#[derive(Debug, Clone)]
pub struct ClaudeLaunch {
    pub model: String,
    pub policy: ClaudeRuntimeQueryPolicy,
    pub native_session: Option<String>,
    pub new_session: Option<String>,
    pub resume_at: Option<String>,
    pub fork: bool,
    pub additional_directories: Vec<String>,
    pub effort: Option<String>,
    pub disallowed_tools: Vec<String>,
    pub mcp_servers: BTreeMap<String, Value>,
    pub settings: Option<Value>,
    pub extra_args: BTreeMap<String, Option<String>>,
}
impl ClaudeLaunch {
    /// Serializable SDK options; callbacks and process I/O belong to the Node worker.
    pub fn sdk_options(&self) -> Value {
        let mut extra = self.extra_args.clone();
        let mode = extra
            .remove("permission-mode")
            .flatten()
            .unwrap_or_else(|| {
                if extra
                    .remove("dangerously-skip-permissions")
                    .is_some_and(|value| value.as_deref().is_none_or(|value| value == "true"))
                {
                    "bypassPermissions".into()
                } else {
                    self.policy.permission_mode.clone()
                }
            });
        extra.remove("dangerously-skip-permissions");
        let summaries = self
            .settings
            .as_ref()
            .is_none_or(|settings| settings["alwaysThinkingEnabled"] != false)
            && extra.get("thinking-display").and_then(Option::as_deref) != Some("omitted");
        let mut options = json!({
            "model": self.model,
            "permissionMode": mode,
            "tools": self.policy.tools.as_ref().map_or_else(|| json!({"type":"preset","preset":"claude_code"}), |tools| json!(tools)),
            "includePartialMessages": true,
            "installPermissionCallback": self.policy.install_permission_callback,
            "additionalDirectories": self.additional_directories,
        });
        if self.policy.allow_dangerously_skip_permissions {
            options["allowDangerouslySkipPermissions"] = json!(true);
        }
        if let Some(tools) = &self.policy.allowed_tools {
            options["allowedTools"] = json!(tools);
        }
        if !self.disallowed_tools.is_empty() {
            options["disallowedTools"] = json!(self.disallowed_tools);
        }
        if let Some(native) = &self.native_session {
            options["resume"] = json!(native);
        } else if let Some(native) = &self.new_session {
            options["sessionId"] = json!(native);
        }
        if let Some(head) = &self.resume_at {
            options["resumeSessionAt"] = json!(head);
        }
        if self.fork {
            options["forkSession"] = json!(true);
        }
        if let Some(effort) = &self.effort {
            options["effort"] = json!(effort);
        }
        if !self.mcp_servers.is_empty() {
            options["mcpServers"] = json!(self.mcp_servers);
        }
        let mut settings = self.settings.clone();
        if summaries {
            options["thinking"] = json!({"type":"adaptive","display":"summarized"});
            if !settings.as_ref().is_some_and(Value::is_string) {
                let mut value = settings.unwrap_or_else(|| json!({}));
                value["showThinkingSummaries"] = json!(true);
                settings = Some(value);
            }
        }
        if let Some(settings) = settings {
            options["settings"] = settings;
        }
        if !extra.is_empty() {
            options["extraArgs"] = json!(extra);
        }
        options
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(resume: bool) -> ClaudeLaunch {
        ClaudeLaunch {
            model: "claude-sonnet-4-6".into(),
            policy: claude_runtime_query_policy(
                RuntimeMode::ApprovalRequired,
                InteractionMode::Default,
                None,
                None,
                false,
            ),
            native_session: resume.then(|| "thinking-thread".into()),
            new_session: (!resume).then(|| "thinking-thread".into()),
            resume_at: None,
            fork: false,
            additional_directories: vec!["/workspace".into()],
            effort: None,
            disallowed_tools: vec![],
            mcp_servers: BTreeMap::new(),
            settings: None,
            extra_args: BTreeMap::new(),
        }
    }
    #[test]
    fn sdk_launch_keeps_thinking_settings_and_native_identity() {
        for resume in [false, true] {
            let selection = launch(resume);
            let options = selection.sdk_options();
            assert_eq!(
                options["thinking"],
                json!({"type":"adaptive","display":"summarized"})
            );
            assert_eq!(
                options["tools"],
                json!({"type":"preset","preset":"claude_code"})
            );
            assert_eq!(options["settings"], json!({"showThinkingSummaries":true}));
            assert_eq!(
                options[if resume { "resume" } else { "sessionId" }],
                "thinking-thread"
            );
            let mut omitted = selection.clone();
            omitted
                .extra_args
                .insert("thinking-display".into(), Some("omitted".into()));
            let options = omitted.sdk_options();
            assert_eq!(options["extraArgs"]["thinking-display"], "omitted");
            assert!(options.get("thinking").is_none());
            assert!(options.get("settings").is_none());
            let mut disabled = selection;
            disabled.settings = Some(json!({"alwaysThinkingEnabled":false}));
            let options = disabled.sdk_options();
            assert!(options.get("thinking").is_none());
            assert_eq!(options["settings"], json!({"alwaysThinkingEnabled":false}));
        }
    }
    #[test]
    fn runtime_policy_preserves_the_reference_permission_and_read_only_cases() {
        use RuntimeMode::*;
        for (runtime, interaction, approval, sandbox, global, mode, callback, skip) in [
            (
                FullAccess,
                InteractionMode::Default,
                Some("never"),
                Some("readOnly"),
                true,
                "dontAsk",
                false,
                false,
            ),
            (
                FullAccess,
                InteractionMode::Default,
                Some("on-request"),
                Some("readOnly"),
                true,
                "default",
                true,
                false,
            ),
            (
                FullAccess,
                InteractionMode::Default,
                Some("never"),
                Some("readOnly"),
                false,
                "dontAsk",
                false,
                false,
            ),
            (
                FullAccess,
                InteractionMode::Default,
                None,
                None,
                false,
                "bypassPermissions",
                false,
                true,
            ),
            (
                Auto,
                InteractionMode::Default,
                None,
                None,
                false,
                "auto",
                false,
                false,
            ),
            (
                ApprovalRequired,
                InteractionMode::Default,
                None,
                Some("dangerFullAccess"),
                false,
                "default",
                true,
                false,
            ),
            (
                ApprovalRequired,
                InteractionMode::Plan,
                None,
                None,
                false,
                "plan",
                true,
                false,
            ),
            (
                FullAccess,
                InteractionMode::Plan,
                None,
                None,
                false,
                "plan",
                false,
                false,
            ),
            (
                ApprovalRequired,
                InteractionMode::Default,
                Some("never"),
                Some("workspaceWrite"),
                false,
                "dontAsk",
                false,
                false,
            ),
            (
                ApprovalRequired,
                InteractionMode::Default,
                Some("never"),
                Some("externalSandbox"),
                false,
                "bypassPermissions",
                false,
                true,
            ),
        ] {
            let policy =
                claude_runtime_query_policy(runtime, interaction, approval, sandbox, global);
            assert_eq!(policy.permission_mode, mode);
            assert_eq!(policy.install_permission_callback, callback);
            assert_eq!(policy.allow_dangerously_skip_permissions, skip);
            assert_eq!(
                policy
                    .tools
                    .as_ref()
                    .map(|tools| tools.iter().map(String::as_str).collect::<Vec<_>>()),
                (sandbox == Some("readOnly")).then_some(vec!["Read", "Glob", "Grep"])
            );
            assert_eq!(
                policy.allowed_tools.is_some(),
                sandbox == Some("readOnly") && global
            );
            let mut options = launch(false);
            options.policy = policy;
            let query = options.sdk_options();
            assert_eq!(query["installPermissionCallback"], callback);
            assert_eq!(
                query["allowDangerouslySkipPermissions"]
                    .as_bool()
                    .unwrap_or(false),
                skip
            );
        }
    }
    #[test]
    fn launch_overrides_compaction_mcp_and_resume_boundary_reach_the_cli() {
        let mut options = launch(true);
        options.resume_at = Some("assistant-boundary".into());
        options.fork = true;
        options.effort = Some("high".into());
        options.settings = Some(json!({"autoCompactWindow":300000}));
        options.mcp_servers.insert(
            "runtime".into(),
            json!({"type":"http","url":"http://127.0.0.1:43123/mcp"}),
        );
        options
            .extra_args
            .insert("dangerously-skip-permissions".into(), None);
        options
            .extra_args
            .insert("permission-mode".into(), Some("plan".into()));
        options
            .extra_args
            .insert("fallback-model".into(), Some("-model".into()));
        let query = options.sdk_options();
        assert_eq!(query["permissionMode"], "plan");
        assert!(
            query["extraArgs"]
                .get("dangerously-skip-permissions")
                .is_none()
        );
        assert_eq!(query["resumeSessionAt"], "assistant-boundary");
        assert_eq!(query["forkSession"], true);
        assert_eq!(query["extraArgs"]["fallback-model"], "-model");
        assert_eq!(query["effort"], "high");
        assert_eq!(
            query["settings"],
            json!({"autoCompactWindow":300000,"showThinkingSummaries":true})
        );
        assert_eq!(query["mcpServers"], json!(options.mcp_servers));
        let mut control = ClaudeControl::default();
        assert_eq!(
            control.initialize("runtime\norchestration")["request"]["appendSystemPrompt"],
            "runtime\norchestration"
        );
    }
}

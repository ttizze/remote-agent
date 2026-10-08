//! The published tool list: the orchestrator, thread and project toolkits with
//! their descriptions, input schemas and annotations.
use serde_json::{Map, Value, json};

/// Tool annotations; the defaults are Effect's.
#[derive(Clone, Copy)]
struct Hints {
    read_only: bool,
    destructive: bool,
    idempotent: bool,
    open_world: bool,
}
const DEFAULT: Hints = Hints {
    read_only: false,
    destructive: true,
    idempotent: false,
    open_world: true,
};
const READ: Hints = Hints {
    read_only: true,
    destructive: false,
    idempotent: true,
    open_world: true,
};
const READ_ONLY: Hints = Hints {
    read_only: true,
    destructive: false,
    idempotent: false,
    open_world: true,
};

fn tool(
    name: &str,
    title: Option<&str>,
    description: &str,
    properties: Value,
    required: &[&str],
    hints: Hints,
) -> Value {
    let mut annotations = Map::new();
    if let Some(title) = title {
        annotations.insert("title".into(), json!(title));
    }
    annotations.insert("readOnlyHint".into(), json!(hints.read_only));
    annotations.insert("destructiveHint".into(), json!(hints.destructive));
    annotations.insert("idempotentHint".into(), json!(hints.idempotent));
    annotations.insert("openWorldHint".into(), json!(hints.open_world));
    json!({
        "name": name,
        "description": description,
        "inputSchema": object(properties, required),
        "annotations": annotations,
    })
}
fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn described(mut schema: Value, description: &str) -> Value {
    schema["description"] = json!(description);
    schema
}
fn string() -> Value {
    json!({"type":"string"})
}
fn text(max: Option<usize>) -> Value {
    match max {
        Some(max) => json!({"type":"string","minLength":1,"maxLength":max}),
        None => json!({"type":"string","minLength":1}),
    }
}
fn int(minimum: u64, maximum: Option<u64>) -> Value {
    match maximum {
        Some(maximum) => json!({"type":"integer","minimum":minimum,"maximum":maximum}),
        None => json!({"type":"integer","minimum":minimum}),
    }
}
fn literals(values: &[&str]) -> Value {
    json!({"type":"string","enum":values})
}
fn prompt() -> Value {
    described(
        text(Some(120_000)),
        "Complete task or message text for the target agent.",
    )
}
fn title() -> Value {
    described(text(Some(512)), "Optional concise display title.")
}
fn client_request_id() -> Value {
    described(
        text(Some(256)),
        "Stable idempotency key to reuse when retrying this mutation.",
    )
}
fn runtime_mode() -> Value {
    literals(&[
        "approval-required",
        "auto-accept-edits",
        "auto",
        "full-access",
    ])
}
fn interaction_mode() -> Value {
    literals(&["default", "plan"])
}
fn option_value() -> Value {
    json!({"anyOf":[{"type":"string","minLength":1},{"type":"boolean"}]})
}
fn option_selections() -> Value {
    json!({"type":"array","items":object(json!({"id":text(None),"value":option_value()}), &["id","value"])})
}
fn target() -> Value {
    object(
        json!({
            "providerInstanceId": described(text(None), "Configured provider instance id from orchestrator_capabilities."),
            "driverKind": described(text(None), "Provider driver kind; prefer providerInstanceId when available."),
            "model": described(text(None), "Model id advertised for the selected provider instance."),
            "options": described(
                json!({"anyOf":[option_selections(),{"type":"object","additionalProperties":option_value()}]}),
                "Model option selections advertised by orchestrator_capabilities.",
            ),
        }),
        &[],
    )
}
fn model_selection() -> Value {
    object(
        json!({
            "instanceId": text(None),
            "model": text(None),
            "options": option_selections(),
        }),
        &["instanceId", "model"],
    )
}
/// A chat image or file attachment; an image's capture source is app-owned.
fn attachment() -> Value {
    let fields = |kind: &str, size: Value| {
        json!({
            "type": literals(&[kind]),
            "id": text(Some(128)),
            "name": text(Some(255)),
            "mimeType": text(Some(100)),
            "sizeBytes": size,
        })
    };
    let mut file = fields("file", int(1, Some(50 * 1024 * 1024)));
    file["source"] = object(json!({"_tag": literals(&["pasted-text"])}), &["_tag"]);
    let required = ["type", "id", "name", "mimeType", "sizeBytes"];
    json!({"anyOf":[
        object(fields("image", int(0, Some(10 * 1024 * 1024))), &required),
        object(file, &required),
    ]})
}
fn source_point() -> Value {
    json!({"anyOf":[
        object(json!({"type":literals(&["latest_stable"])}), &["type"]),
        object(json!({"type":literals(&["run"]),"runId":string()}), &["type","runId"]),
        object(json!({"type":literals(&["checkpoint"]),"checkpointId":string()}), &["type","checkpointId"]),
    ]})
}
fn thread_status() -> Value {
    literals(&[
        "idle",
        "preparing",
        "queued",
        "starting",
        "running",
        "waiting",
        "completed",
        "interrupted",
        "failed",
        "cancelled",
        "rolled_back",
    ])
}
fn optional_thread() -> Value {
    json!({"threadId": string()})
}

fn scheduled_schedule() -> Value {
    json!({
        "anyOf": [
            object(
                json!({
                    "type": literals(&["interval"]),
                    "everyMs": int(60_000, None),
                }),
                &["type", "everyMs"],
            ),
            object(
                json!({
                    "type": literals(&["fixed_time"]),
                    "timeOfDay": {
                        "type":"string",
                        "minLength":4,
                        "maxLength":5,
                        "pattern":"^([01]?\\d|2[0-3]):([0-5]\\d)$"
                    },
                    "weekdays": {"type":"array", "items":int(0, Some(6)), "maxItems":7},
                }),
                &["type", "timeOfDay"],
            ),
        ],
    })
}

fn orchestrator() -> Vec<Value> {
    vec![
        tool(
            "orchestrator_capabilities",
            Some("Get orchestration capabilities"),
            "List the V2 provider instances and their current models from the same live catalog as the composer, including configured custom models, inherited runtime settings, and app-owned orchestration features available to this thread. For a separate top-level thread in a new or existing worktree, use thread_launch with workspaceStrategy.",
            json!({}),
            &[],
            READ,
        ),
        tool(
            "delegate_task",
            Some("Delegate a child task"),
            "Delegate one task to a app-owned child agent/subagent of THIS thread and run it with only the supplied task prompt, without copying parent conversation history. Choose providers and models from orchestrator_capabilities, which uses the same live catalog as the composer. Prefer native subagent tools for same-provider work only when they support the chosen model. Use this for any model missing from the native tool, including same-provider work, for cross-provider work, or for explicitly app-owned child tasks. For every delegated review round, call delegate_task again with the original brief, prior findings, responses, and unresolved objections in the task prompt. Track each round by its own taskId and use a distinct clientRequestId per round, stable across retries of that round. The childThreadId is backing storage, not the target for starting another delegated review round through thread_send. Provider, model, model options (see orchestrator_capabilities), runtime mode, and interaction mode inherit unless target overrides them. Prefer mode='async' for long work; mode='wait' blocks until completion or timeout. timeoutMs on mode=wait is only the parent's wait budget and does not cancel the child. waitTimedOut on that wait call means the timeout fired; keep that taskId and read status on later task_status. An async child's completion wakes this thread through a notification, steered into active turns where supported or queued otherwise, so end the turn instead of polling or spawning watchers; use task_status only when the result is needed mid-turn.",
            json!({
                "task": described(text(Some(120_000)), "Self-contained task for one delegated child agent/subagent."),
                "target": target(),
                "title": title(),
                "role": literals(&["implementation","research","review","design","test","general"]),
                "mode": described(literals(&["async","wait"]), "Defaults to async. Use wait only when this turn needs the child's result before you can continue."),
                "timeoutMs": described(json!({"type":"number"}), "Wait budget for mode=wait only. Default 10 minutes. Elapsing it returns waitTimedOut=true on that call and does not cancel the child."),
                "clientRequestId": client_request_id(),
                "runtimeMode": json!({"type":"string","enum":["inherit","approval-required","auto-accept-edits","auto","full-access"]}),
                "interactionMode": literals(&["inherit","default","plan"]),
            }),
            &["task"],
            Hints {
                open_world: true,
                ..DEFAULT
            },
        ),
        tool(
            "task_status",
            Some("Get delegated task status"),
            "Read a app-owned delegated task created by this parent thread. childRunId identifies the original delegated run. workState distinguishes working, waiting_for_children, and result_available; a completed turn with live nested work is not a completed task. summary is the final task result, including provider errors on failure, and remains stable after publication. hasPendingChildRuns reports later queued or executing turns in the backing child thread, even after the task is terminal; it does not reopen the task or extend task_cancel to those turns. latestTerminal* provides later non-monitor turn results. Reading a terminal result acknowledges its automatic parent delivery.",
            json!({"taskId": string()}),
            &["taskId"],
            Hints {
                read_only: false,
                destructive: false,
                idempotent: true,
                open_world: true,
            },
        ),
        tool(
            "task_cancel",
            Some("Cancel delegated task"),
            "Request interruption of an active app-owned delegated task and dispose its automatic parent delivery. For a terminal task, return its existing status and dispose delivery without interrupting later child-thread runs, even when task_status reports hasPendingChildRuns=true. Published task results remain available. Use thread_interrupt for a later active run.",
            json!({
                "taskId": string(),
                "reason": json!({"type":"string","maxLength":2000}),
                "clientRequestId": client_request_id(),
            }),
            &["taskId"],
            DEFAULT,
        ),
        tool(
            "schedule_task",
            Some("Schedule a recurring task"),
            "Create persistent recurring work in the Host scheduler, which runs even when no turn is active. Pass schedule as a STRUCTURED OBJECT, never JSON text: {type:'interval', everyMs:3600000} means hourly; {type:'fixed_time', timeOfDay:'09:00', weekdays:[1,2,3,4,5]} means weekday mornings. By default (bindToCurrentThread=true) each run posts into the calling thread; use false only when a fresh top-level thread per run is wanted. Provider, model and runtime settings inherit from the calling thread. Report the returned schedule and nextRunAt after success.",
            json!({
                "prompt": prompt(),
                "schedule": scheduled_schedule(),
                "title": title(),
                "enabled": {"type":"boolean"},
                "bindToCurrentThread": {"type":"boolean"},
                "clientRequestId": client_request_id(),
            }),
            &["prompt", "schedule"],
            DEFAULT,
        ),
        tool(
            "list_scheduled_tasks",
            Some("List scheduled tasks"),
            "List recurring scheduled tasks in the calling thread's project, including their id, schedule, prompt, enabled state, bound thread, next run time and last run status. Use the returned scheduledTaskId with update_scheduled_task or delete_scheduled_task.",
            json!({}),
            &[],
            READ,
        ),
        tool(
            "update_scheduled_task",
            Some("Update a scheduled task"),
            "Update an existing scheduled task by scheduledTaskId from list_scheduled_tasks. Only provided fields change; omit a field to leave it unchanged. Use enabled=false to pause a task without deleting it, or bindToCurrentThread to move it between posting into this thread and launching a fresh thread per run.",
            json!({
                "scheduledTaskId": text(None),
                "prompt": prompt(),
                "title": title(),
                "schedule": scheduled_schedule(),
                "enabled": {"type":"boolean"},
                "bindToCurrentThread": {"type":"boolean"},
            }),
            &["scheduledTaskId"],
            DEFAULT,
        ),
        tool(
            "delete_scheduled_task",
            Some("Delete a scheduled task"),
            "Permanently delete a scheduled task by scheduledTaskId from list_scheduled_tasks. The task stops running immediately. To keep it but stop runs, use update_scheduled_task with enabled=false instead.",
            json!({"scheduledTaskId": text(None)}),
            &["scheduledTaskId"],
            DEFAULT,
        ),
        tool(
            "create_threads",
            Some("Create threads"),
            "Create one or more ORDINARY TOP-LEVEL conversations. This is not delegation and does not create child agents/subagents. For delegated work, choose models from orchestrator_capabilities. Prefer native subagents only when they support the chosen model; otherwise call delegate_task, including for same-provider work. Use create_threads for a batch of separate top-level threads sharing this checkout. Prefer thread_launch for a single thread. Both require the user to request separate/new/top-level threads or conversations. Each entry may override provider, model, options, runtime mode, and interaction mode; omitted settings inherit. Project, branch, and worktree always inherit and cannot be overridden here. For independent implementation or a PR stack in its own worktree, use thread_launch with workspaceStrategy instead of asking the agent to create a worktree in its prompt.",
            json!({
                "threads": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 20,
                    "items": object(json!({
                        "prompt": prompt(),
                        "title": title(),
                        "target": target(),
                        "runtimeMode": json!({"type":"string","enum":["inherit","approval-required","auto-accept-edits","auto","full-access"]}),
                        "interactionMode": literals(&["inherit","default","plan"]),
                    }), &[]),
                },
                "clientRequestId": client_request_id(),
            }),
            &["threads"],
            Hints {
                open_world: true,
                ..DEFAULT
            },
        ),
        tool(
            "thread_list",
            Some("List threads"),
            "List threads in the calling thread's project, newest first. Filter by durable run status, title, or settled state (settled=true lists threads the user or auto-settlement moved out of the active list) and paginate with the returned cursor. Threads from other projects are never exposed.",
            json!({
                "statuses": {"type":"array","items":thread_status(),"maxItems":10},
                "titleContains": text(Some(256)),
                "settled": {"type":"boolean"},
                "includeSubagents": {"type":"boolean"},
                "cursor": int(0, None),
                "limit": int(1, Some(100)),
            }),
            &[],
            READ,
        ),
        tool(
            "thread_read",
            Some("Read a thread"),
            "Read durable state and a paginated timeline from a thread in the calling project, or from a thread the user attached to this conversation as context. The default messages view returns user messages, assistant messages, and proposed plans; activity returns all summarized timeline items. Reading an untruncated terminal assistant result from this parent thread's direct app-owned child acknowledges that child's automatic completion delivery. Continue with afterPosition=nextPosition. Recover long item text with itemId and textOffset=nextTextOffset until nextTextOffset is null; offsets count UTF-16 code units.",
            json!({
                "threadId": string(),
                "itemId": string(),
                "textOffset": int(0, None),
                "view": literals(&["messages","activity"]),
                "afterPosition": int(0, None),
                "limit": int(1, Some(100)),
                "runLimit": int(1, Some(50)),
                "maxCharsPerItem": int(1, Some(50_000)),
            }),
            &["threadId"],
            Hints {
                read_only: false,
                destructive: false,
                idempotent: true,
                open_world: true,
            },
        ),
        tool(
            "thread_update",
            Some("Update thread metadata"),
            "Update metadata for a thread in the calling project. Omit threadId to update this thread. Use action='rename' with title, action='regenerate_title' with no extra field, action='link_pull_request' with pullRequest, or action='unlink_pull_request'. Workspace and branch changes are intentionally not supported. clientRequestId makes retries idempotent.",
            json!({
                "threadId": described(string(), "Thread in the calling project. Omit to update the calling thread."),
                "action": described(
                    literals(&["rename","regenerate_title","link_pull_request","unlink_pull_request"]),
                    "Metadata mutation: rename, regenerate_title, link_pull_request, or unlink_pull_request.",
                ),
                "title": described(text(Some(512)), "New concise display title. Required only when action is rename."),
                "pullRequest": described(
                    object(json!({
                        "repository": described(text(None), "Repository name as owner/name."),
                        "number": described(int(1, None), "Pull request number."),
                        "url": described(text(None), "Canonical HTTP(S) pull request URL, including self-hosted repository URLs."),
                    }), &["repository","number","url"]),
                    "Pull request to link. Required only when action is link_pull_request.",
                ),
                "clientRequestId": client_request_id(),
            }),
            &["action"],
            Hints {
                destructive: true,
                idempotent: false,
                ..DEFAULT
            },
        ),
        tool(
            "thread_send",
            Some("Send to a thread"),
            "Send a message to a thread in the calling project. Do not use a delegated task's childThreadId to start another review round here; use delegate_task with the full review context and a new clientRequestId for that round. Thread messages do not create a new delegated task or reopen a completed task. mode='auto' starts an idle thread, steers a fully active turn, or queues behind a turn that is not yet steerable. Use queue for a separate follow-up turn, steer for an in-flight update, or restart to interrupt-and-restart the active turn. clientRequestId makes retries idempotent.",
            json!({
                "threadId": string(),
                "message": prompt(),
                "mode": literals(&["auto","queue","steer","restart"]),
                "clientRequestId": client_request_id(),
            }),
            &["threadId", "message"],
            Hints {
                open_world: true,
                ..DEFAULT
            },
        ),
        tool(
            "thread_wait",
            Some("Wait for a thread"),
            "Wait for a thread run to reach a terminal durable state. Without runId, the latest run at call time is selected; an idle thread returns immediately. Timeout does not interrupt work, so call again or use thread_read/list after timedOut=true. Waiting reports status only and does not acknowledge a delegated result.",
            json!({
                "threadId": string(),
                "runId": string(),
                "timeoutMs": {"type":"number"},
            }),
            &["threadId"],
            READ,
        ),
        tool(
            "thread_interrupt",
            Some("Interrupt a thread"),
            "Request interruption of a running turn in a thread in the calling project. Without runId, the newest interruptible run is selected. Terminal runs and threads without an active turn return without another side effect. clientRequestId makes retries idempotent.",
            json!({
                "threadId": string(),
                "runId": string(),
                "reason": json!({"type":"string","maxLength":2000}),
                "clientRequestId": client_request_id(),
            }),
            &["threadId"],
            DEFAULT,
        ),
    ]
}

fn thread() -> Vec<Value> {
    let queue_target = |extra: Value, required: &[&str]| {
        let mut properties = json!({"threadId": string(), "queuedRunId": string()});
        if let (Some(properties), Value::Object(extra)) = (properties.as_object_mut(), extra) {
            properties.extend(extra);
        }
        let mut required = required.to_vec();
        required.insert(0, "queuedRunId");
        object(properties, &required)
    };
    let with_schema = |name: &str, description: &str, schema: Value, hints: Hints| {
        let mut definition = tool(name, None, description, json!({}), &[], hints);
        definition["inputSchema"] = schema;
        definition
    };
    vec![
        tool(
            "run_scheduled_task_now",
            Some("Run a scheduled task"),
            "Run a scheduled task in the calling project now through the existing scheduler. Requires a full-access/default caller. Each call is a new manual run; completion means dispatch and bookkeeping completed, not that the provider turn finished.",
            json!({"taskId": text(None)}),
            &["taskId"],
            DEFAULT,
        ),
        tool(
            "thread_search",
            None,
            "Search active thread titles and content with the app's existing bounded search. Returns matches in the calling project from the global top matches; other-project matches are omitted, so this may return fewer than limit. No pagination or exhaustive-result guarantee.",
            json!({
                "query": {"type":"string","minLength":2,"maxLength":200},
                "limit": int(1, Some(50)),
            }),
            &["query"],
            READ_ONLY,
        ),
        tool(
            "thread_fork",
            None,
            "Fork this thread from a stable run or checkpoint using the existing fork command. The fork inherits the source configuration. Acceptance does not mean a provider turn has completed.",
            json!({"sourcePoint": source_point(), "title": text(None)}),
            &["sourcePoint"],
            DEFAULT,
        ),
        tool(
            "thread_merge_back",
            None,
            "Merge context from this thread back to a related thread in the same project. Existing lineage and transfer rules apply.",
            json!({"targetThreadId": string(), "sourcePoint": source_point()}),
            &["targetThreadId", "sourcePoint"],
            DEFAULT,
        ),
        tool(
            "thread_transfers",
            None,
            "Read context transfer status for a thread in the calling project.",
            optional_thread(),
            &[],
            READ_ONLY,
        ),
        tool(
            "thread_configuration",
            None,
            "Read a thread's provider/model selection and modes in the calling project. orchestrator_capabilities lists available providers and models.",
            optional_thread(),
            &[],
            READ_ONLY,
        ),
        tool(
            "thread_configure",
            None,
            "Set this calling thread's provider, model and options with the existing selection command. This does not change permission modes or other threads. Use orchestrator_capabilities to choose a selection.",
            json!({"modelSelection": model_selection()}),
            &["modelSelection"],
            DEFAULT,
        ),
        tool(
            "pending_request_list",
            None,
            "List pending user questions in a thread in the calling project. Approval requests are not included.",
            optional_thread(),
            &[],
            READ_ONLY,
        ),
        tool(
            "pending_request_read",
            None,
            "Read a pending user question. Answer with pending_request_respond; existing live or message response handling is used.",
            json!({"threadId": string(), "requestId": string()}),
            &["requestId"],
            READ_ONLY,
        ),
        tool(
            "pending_request_respond",
            None,
            "Answer a pending user-input request using the existing runtime response command. This cannot approve a permission request.",
            json!({"threadId": string(), "requestId": string(), "answers": {"type":"object"}}),
            &["requestId", "answers"],
            Hints {
                open_world: true,
                ..DEFAULT
            },
        ),
        tool(
            "thread_organize",
            Some("Organize a thread"),
            "Pin, snooze, settle, archive, or mark a thread unread in the calling project. Omit threadId for this thread. snooze requires snoozedUntil. Existing thread lifecycle rules apply; this does not schedule a future action.",
            json!({
                "threadId": string(),
                "action": literals(&["pin","unpin","snooze","unsnooze","settle","unsettle","archive","unarchive","mark_unread"]),
                "snoozedUntil": {"type":"string","format":"date-time"},
            }),
            &["action"],
            DEFAULT,
        ),
        tool(
            "queue_list",
            None,
            "List queued messages in delivery order. Results are a live offset page; use thread_read for full thread history.",
            json!({"threadId": string(), "cursor": int(0, None), "limit": int(1, Some(100))}),
            &[],
            READ_ONLY,
        ),
        with_schema(
            "queue_read",
            "Read up to 16,000 characters of a queued message in the calling project.",
            queue_target(json!({}), &[]),
            READ_ONLY,
        ),
        with_schema(
            "queue_edit",
            "Replace a queued message's text, preserving its attachments. The service rejects runs that are no longer queued.",
            queue_target(
                json!({"text": {"type":"string","maxLength":100_000}}),
                &["text"],
            ),
            DEFAULT,
        ),
        with_schema(
            "queue_cancel",
            "Cancel a queued run using the existing queue command.",
            queue_target(json!({}), &[]),
            DEFAULT,
        ),
        with_schema(
            "queue_reorder",
            "Move a queued run before another queued run, or to the end with beforeRunId=null.",
            queue_target(
                json!({"beforeRunId": {"anyOf":[string(),{"type":"null"}]}}),
                &["beforeRunId"],
            ),
            DEFAULT,
        ),
        with_schema(
            "queue_promote_to_steer",
            "Deliver a queued message as steering to the specified active run. Existing provider and run-state rules apply.",
            queue_target(json!({"targetRunId": string()}), &["targetRunId"]),
            DEFAULT,
        ),
    ]
}

fn project() -> Vec<Value> {
    vec![
        tool(
            "thread_launch",
            None,
            "Create an ordinary TOP-LEVEL thread with an explicit workspace binding before its agent starts. Use this when the user requests independent work, a new thread, or a PR stack in its own worktree; use delegate_task for child subagents. Set workspaceStrategy to {type:\"worktree\",baseRef:\"parent-branch\",branch:\"new-branch\",startFromOrigin:false} for a new worktree based on local commits, or {type:\"existing_worktree\",worktreePath:\"/absolute/path\",branch:\"existing-branch\"} to use an existing checkout. For upstream commits, set startFromOrigin:true. Omitted workspaceStrategy means the project root, NOT the caller's worktree. Omit projectId/modelSelection/modes to inherit those settings. Set scratch:true instead of projectId for a thread without a project: it runs in a fresh folder of its own, outside any repository. Put the task in message. Do not ask the agent to create its own worktree via shell: that does not update the thread binding. Each call creates a new launch with no retry key; retain threadId and use thread_read/thread_wait to follow preparation. After errors or lost responses, inspect thread_list before retrying. Attachments must be pending uploads. Requires a full-access/default caller.",
            json!({
                "projectId": string(),
                "scratch": described(json!({"type":"boolean"}), "Launch without a project, in its own folder under the environment's Scratch project. Not with projectId or workspaceStrategy."),
                "title": text(None),
                "modelSelection": model_selection(),
                "runtimeMode": runtime_mode(),
                "interactionMode": interaction_mode(),
                "workspaceStrategy": described(
                    json!({"anyOf":[
                        object(json!({"type":literals(&["root"]),"branch":text(None)}), &["type"]),
                        object(json!({"type":literals(&["existing_worktree"]),"worktreePath":text(None),"branch":text(None)}), &["type","worktreePath"]),
                        object(json!({"type":literals(&["worktree"]),"baseRef":text(None),"branch":text(None),"startFromOrigin":{"type":"boolean"}}), &["type","baseRef"]),
                    ]}),
                    "Choose where this thread runs before starting its agent: worktree creates and binds a new checkout from baseRef; existing_worktree binds worktreePath; root uses the project checkout. Omitted means root, not the caller's worktree. For a PR stack use the parent branch as baseRef and startFromOrigin:false. Uncommitted changes are not copied.",
                ),
                "message": described(
                    json!({"type":"string","maxLength":120_000}),
                    "First task prompt, delivered after workspace preparation. Omit message and attachments to create an idle thread.",
                ),
                "attachments": {"type":"array","maxItems":8,"items":attachment()},
            }),
            &["title"],
            Hints {
                open_world: true,
                ..DEFAULT
            },
        ),
        tool(
            "project_list",
            None,
            "List registered projects in this environment. Pages use the current project snapshot and may shift between calls.",
            json!({"cursor": int(0, None), "limit": int(1, Some(100))}),
            &[],
            READ_ONLY,
        ),
        tool(
            "project_read",
            None,
            "Read a registered project in this environment, including its workspace and saved scripts.",
            json!({"projectId": string()}),
            &["projectId"],
            READ_ONLY,
        ),
        tool(
            "project_create",
            None,
            "Register a project directory through the existing project service. Set createWorkspaceRootIfMissing to create a directory. Omit workspaceRoot to start a new project from just its title: the app makes a Git repository for it in its own projects folder, with a README, an icon, and a first commit (commitError says why a commit failed; the project exists either way). Each call creates a new request; an existing registered workspace is rejected.",
            json!({
                "title": text(None),
                "workspaceRoot": text(None),
                "createWorkspaceRootIfMissing": {"type":"boolean"},
                "defaultModelSelection": {"anyOf":[model_selection(),{"type":"null"}]},
                "scripts": {"type":"array","items":{"type":"object"}},
            }),
            &["title"],
            DEFAULT,
        ),
    ]
}

fn worktree() -> Vec<Value> {
    vec![
        tool(
            "worktree_handoff",
            Some("Hand off thread to a Git worktree"),
            "Move this agent thread into a new Git worktree. Creates the branch, records the thread binding, and optionally runs the project's setup script there. Changing the workspace detaches the current provider session; pass continuationPrompt to queue the remaining work as the next turn inside the worktree. The worktree is not removed automatically when the thread is deleted. Fails when the thread is already attached to a worktree.",
            json!({
                "branch": described(text(Some(512)), "Branch name for the new worktree."),
                "baseRef": described(text(Some(512)), "Branch or ref to start from; defaults to the current branch."),
                "startFromOrigin": described(json!({"type":"boolean"}), "Fetch the primary remote before creating the worktree; defaults to the Host setting."),
                "path": described(text(Some(4096)), "Absolute path for the checkout; defaults to the Host-managed worktree directory."),
                "runSetupScript": described(json!({"type":"boolean"}), "Run the project's configured setup script after binding; defaults to true."),
                "continuationPrompt": prompt(),
            }),
            &["branch"],
            Hints {
                open_world: true,
                ..DEFAULT
            },
        ),
        tool(
            "worktree_status",
            Some("Get thread worktree status"),
            "Report whether this thread is attached to a Git worktree, its path and branch, the project workspace root, and the Host default used by worktree_handoff.",
            json!({}),
            &[],
            READ,
        ),
        tool(
            "worktree_list",
            Some("List workspace branches"),
            "List branch refs and their checkout paths for this thread's project workspace. Detached checkouts are not included. Use worktree_status for this thread's binding and worktree_handoff to create a new checkout.",
            json!({
                "query": text(Some(256)),
                "cursor": int(0, None),
                "limit": int(1, Some(200)),
                "refKind": literals(&["all", "local", "remote"]),
                "includeMatchingRemoteRefs": {"type":"boolean"},
            }),
            &[],
            READ,
        ),
    ]
}

/// Every served tool.
pub(crate) fn tools() -> Vec<Value> {
    [orchestrator(), thread(), project(), worktree(), diagnostics(), device()].concat()
}

fn diagnostics() -> Vec<Value> {
    vec![
        tool(
            "background_status",
            Some("Read background activity"),
            "Read the Host-owned background activity leases, power state, and opportunistic work gate.",
            json!({}),
            &[],
            READ,
        ),
        tool(
            "host_resources",
            Some("Read Host resources"),
            "Read one demand-driven local Host resource sample owned by the Host process.",
            json!({}),
            &[],
            READ,
        ),
        tool(
            "process_diagnostics",
            Some("Read process diagnostics"),
            "Read signalable child, provider, and terminal process diagnostics from the local Host sample.",
            json!({}),
            &[],
            READ,
        ),
        tool(
            "process_resource_history",
            Some("Read process resource history"),
            "Read bounded local process resource history for the requested window and bucket size.",
            json!({
                "windowMs": int(1_000, Some(3_600_000)),
                "bucketMs": int(1_000, Some(3_600_000)),
            }),
            &[],
            READ,
        ),
        tool(
            "trace_diagnostics",
            Some("Read trace diagnostics"),
            "Read bounded local trace and warning records from the Host diagnostics directory.",
            json!({
                "traceFilePath": string(),
                "maxFiles": int(0, Some(16)),
                "slowSpanThresholdMs": {"type":"number","minimum":0},
            }),
            &[],
            READ,
        ),
    ]
}

/// The tools annotated read-only, which a read-only Claude sandbox pre-approves.
pub(crate) fn read_only_tools() -> Vec<String> {
    tools()
        .into_iter()
        .filter(|tool| tool["annotations"]["readOnlyHint"] == true)
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}
fn device() -> Vec<Value> {
    vec![
        tool(
            "device_list",
            Some("List devices"),
            "List simulator and emulator device hosts, available devices, and devices open in this thread's Device panel. Call this before device_open when the id is unknown.",
            json!({"hostId": described(string(), "Limit discovery to one device host.")}),
            &[],
            READ,
        ),
        tool(
            "device_open",
            Some("Open device"),
            "Open a simulator or emulator for this thread, booting it when needed, and show it in the user's Device panel. The result includes the pinned agent-device target arguments.",
            json!({
                "hostId": described(string(), "Device host from device_list. Defaults to local."),
                "deviceId": described(string(), "Simulator UDID or emulator serial from device_list. Omit to choose the booted device."),
                "platform": described(literals(&["ios", "android"]), "Required when deviceId is omitted and both platforms are available."),
            }),
            &[],
            Hints { destructive: false, idempotent: true, ..DEFAULT },
        ),
        tool(
            "device_screenshot",
            Some("Screenshot device"),
            "Capture the current screen of an open device as a PNG image. Omit deviceId to use the most recently opened device in this thread.",
            json!({
                "hostId": described(string(), "Device host. Defaults to local."),
                "deviceId": described(string(), "Device from device_list. Omit to use the most recently opened device in this thread."),
            }),
            &[],
            READ,
        ),
        tool(
            "device_close",
            Some("Close device"),
            "Remove a device from this thread's Device panel. Set shutdown to also power the simulator or emulator off.",
            json!({
                "hostId": described(string(), "Device host. Defaults to local."),
                "deviceId": described(string(), "Device to close. Omit to close every device in this thread."),
                "shutdown": described(json!({"type":"boolean"}), "Also power the simulator or emulator off. Defaults to false."),
            }),
            &[],
            Hints { idempotent: true, ..DEFAULT },
        ),
    ]
}

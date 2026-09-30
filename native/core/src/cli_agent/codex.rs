//! OpenAI Codex adapters.
//!
//! Primary: `codex app-server` — a long-lived JSON-RPC 2.0 peer over stdio
//! (`initialize` → `initialized` → `thread/start` → `turn/start`, streamed
//! `item/*` notifications, server→client approval requests such as
//! `item/commandExecution/requestApproval`, and `turn/interrupt`). Shapes
//! follow `codex app-server generate-json-schema` (v2 protocol).
//! Fallback: `codex exec --json` — one-shot JSONL events (`thread.started`,
//! `item.started`, `item.completed`, `turn.completed`, `error`) with no
//! approval channel; only used when app-server is unavailable.
use super::{
    clip, redact, redact_value, ApprovalPrompt, CliAdapter, LaunchOptions, PromptImage, Step,
    Update, Vendor, VendorAnswer,
};

/// The composer's reasoning effort, when it is one Codex accepts.
fn effort(options: &LaunchOptions) -> Option<&str> {
    options
        .effort
        .as_deref()
        .filter(|effort| matches!(*effort, "low" | "medium" | "high"))
}

/// `-c model_reasoning_effort="…"` (Codex's documented config key), placed
/// before the subcommand. It sets the effort of threads the process starts;
/// a resumed app-server thread keeps its stored effort, so app-server turns
/// also carry `turn/start.effort`.
fn effort_override(options: &LaunchOptions) -> Vec<String> {
    match effort(options) {
        Some(effort) => vec!["-c".into(), format!("model_reasoning_effort=\"{effort}\"")],
        None => Vec::new(),
    }
}
use anyhow::{bail, Result};
use serde_json::{json, Value};

const OUTPUT_PREVIEW: usize = 8000;
// Only the matching turn/start response establishes turn identity. Some peers
// can interleave scoped notifications before that response; retain a bounded
// prefix, then validate every envelope before publishing it to the host task.
const MAX_EARLY_FRAMES: usize = 128;
const MAX_EARLY_BYTES: usize = 1024 * 1024;

fn rpc_request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string()
}
fn rpc_notification(method: &str, params: Option<Value>) -> String {
    match params {
        Some(params) => json!({"jsonrpc":"2.0","method":method,"params":params}).to_string(),
        None => json!({"jsonrpc":"2.0","method":method}).to_string(),
    }
}
fn rpc_result(id: &Value, result: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"result":result}).to_string()
}
fn rpc_error(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}).to_string()
}

/// Approval ids are the JSON-RPC request ids, which may be numbers or strings.
/// They are carried as their JSON text so they round-trip exactly.
fn request_id_key(id: &Value) -> String {
    id.to_string()
}
fn request_id_value(key: &str) -> Value {
    serde_json::from_str(key).unwrap_or_else(|_| Value::String(key.to_owned()))
}

#[derive(Debug, Default, PartialEq)]
enum Phase {
    #[default]
    Starting,
    Initialized,
    ThreadStarted,
}

/// `codex app-server` translator.
#[derive(Default)]
pub struct CodexAppServerAdapter {
    next_id: u64,
    phase: Phase,
    init_id: Option<u64>,
    thread_start_id: Option<u64>,
    turn_start_id: Option<u64>,
    interrupt_id: Option<u64>,
    thread_id: Option<String>,
    turn_id: Option<String>,
    pending_prompt: Option<(String, Vec<PromptImage>)>,
    pending_approvals: std::collections::HashMap<String, String>,
    streamed_message_ids: std::collections::HashSet<String>,
    /// The agent message whose text was shown last in this turn. The next
    /// message (Codex's commentary, then its final answer) starts a new
    /// paragraph instead of running into it.
    last_text_item: Option<String>,
    options: Option<LaunchOptions>,
    turn_active: bool,
    resume_failed: bool,
    early_frames: std::collections::VecDeque<Value>,
    early_bytes: usize,
}
impl CodexAppServerAdapter {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
    fn start_turn(&mut self, text: &str, images: &[PromptImage]) -> Result<Vec<String>> {
        let Some(thread) = self.thread_id.clone() else {
            bail!("Codex thread is not ready")
        };
        let mut send = self.retire_turn("The Codex turn was replaced");
        let id = self.id();
        self.turn_start_id = Some(id);
        self.turn_active = true;
        let mut input = vec![json!({"type":"text","text":text})];
        for image in images {
            input.push(json!({
                "type":"localImage",
                "path": image.absolute_path,
            }));
        }
        let mut params = json!({"threadId":thread,"input":input});
        if let Some(options) = &self.options {
            params["cwd"] = json!(options.workspace);
            // "Override the reasoning effort for this turn and subsequent
            // turns": the only effort control a resumed thread honours
            // (Codex 0.158 keeps a resumed thread's stored effort even when
            // the process was started with `-c model_reasoning_effort`).
            if let Some(effort) = effort(options) {
                params["effort"] = json!(effort);
            }
        }
        send.push(rpc_request(id, "turn/start", params));
        Ok(send)
    }
    fn retire_turn(&mut self, reason: &str) -> Vec<String> {
        self.turn_id = None;
        self.turn_start_id = None;
        self.turn_active = false;
        self.streamed_message_ids.clear();
        self.last_text_item = None;
        self.early_bytes = 0;
        let mut send: Vec<_> = self
            .early_frames
            .drain(..)
            .filter_map(|frame| frame.get("id").cloned())
            .map(|id| rpc_error(&id, -32602, reason))
            .collect();
        send.extend(
            self.pending_approvals
                .drain()
                .map(|(id, _)| rpc_error(&request_id_value(&id), -32602, reason)),
        );
        send
    }
    fn reject_scope(message: &Value) -> Step {
        Step {
            send: message
                .get("id")
                .filter(|id| !id.is_null())
                .map(|id| {
                    vec![rpc_error(
                        id,
                        -32602,
                        "Codex event does not match the active thread and turn",
                    )]
                })
                .unwrap_or_default(),
            updates: vec![Update::Warning(
                "Ignored a Codex event outside the active thread and turn".into(),
            )],
        }
    }
    /// Validate only identity fields from the installed v2 schema. Account
    /// notices remain global; this is not a complete schema validator.
    fn scope_message(&mut self, message: &Value) -> Option<Step> {
        let method = message["method"].as_str()?;
        let params = &message["params"];
        let modern = matches!(
            method,
            "item/agentMessage/delta"
                | "item/started"
                | "item/completed"
                | "thread/tokenUsage/updated"
                | "error"
                | "turn/started"
                | "turn/completed"
                | "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/permissions/requestApproval"
        );
        let legacy = matches!(method, "execCommandApproval" | "applyPatchApproval");
        let thread_warning =
            method == "guardianWarning" || (method == "warning" && !params["threadId"].is_null());
        if !modern && !legacy && !thread_warning {
            return None;
        }
        let thread = params[if legacy { "conversationId" } else { "threadId" }]
            .as_str()
            .filter(|id| !id.is_empty());
        if thread.is_none() || thread != self.thread_id.as_deref() {
            return Some(Self::reject_scope(message));
        }
        if thread_warning {
            return None;
        }
        // A resumed thread repeats its previous turn's token usage right
        // after `thread/resume` (Codex 0.158). That notification carries no
        // authority and belongs to an earlier turn: drop it quietly instead
        // of warning on every follow-up, and never count it.
        let stale_usage =
            method == "thread/tokenUsage/updated" && message.get("id").is_none_or(Value::is_null);
        if !self.turn_active {
            return Some(if stale_usage {
                Step::default()
            } else {
                Self::reject_scope(message)
            });
        }
        // Legacy approvals have no turn field. Accept only after a current
        // turn is authoritatively bound, and only for its matching thread.
        if legacy {
            return self.turn_id.is_none().then(|| Self::reject_scope(message));
        }
        let turn = if matches!(method, "turn/started" | "turn/completed") {
            &params["turn"]["id"]
        } else {
            &params["turnId"]
        };
        let Some(turn) = turn.as_str().filter(|id| !id.is_empty()) else {
            return Some(Self::reject_scope(message));
        };
        if let Some(owned) = self.turn_id.as_deref() {
            return (turn != owned).then(|| {
                if stale_usage {
                    Step::default()
                } else {
                    Self::reject_scope(message)
                }
            });
        }
        if self.turn_start_id.is_none() {
            return Some(Self::reject_scope(message));
        }
        let bytes = message.to_string().len();
        if self.early_frames.len() >= MAX_EARLY_FRAMES
            || bytes > MAX_EARLY_BYTES.saturating_sub(self.early_bytes)
        {
            let mut send = self.retire_turn("Codex early-event buffer exceeded its limit");
            if let Some(id) = message.get("id").filter(|id| !id.is_null()) {
                send.push(rpc_error(
                    id,
                    -32602,
                    "Codex early-event buffer exceeded its limit",
                ));
            }
            return Some(Step {send, updates: vec![Update::TurnFailed(
                "Codex sent too many events before identifying the active turn (128 frames / 1 MiB limit)".into())]});
        }
        self.early_bytes += bytes;
        self.early_frames.push_back(message.clone());
        Some(Step::default())
    }
    fn dispatch_scoped(&mut self, message: &Value) -> Step {
        if let Some(id) = message.get("id").filter(|id| !id.is_null()) {
            let key = request_id_key(id);
            if self.pending_approvals.contains_key(&key)
                || self
                    .early_frames
                    .iter()
                    .any(|frame| frame.get("id") == Some(id))
            {
                return Step {
                    send: self.retire_turn("Codex reused an outstanding server request id"),
                    updates: vec![Update::TurnFailed(
                        "Codex reused an outstanding server request id; approval authority was retired".into()
                    )],
                };
            }
        }
        if let Some(step) = self.scope_message(message) {
            return step;
        }
        let Some(method) = message["method"].as_str() else {
            return Step::default();
        };
        match message.get("id").filter(|id| !id.is_null()) {
            Some(id) => self.handle_server_request(id, method, &message["params"]),
            None => self.handle_notification(method, &message["params"]),
        }
    }
    fn handle_response(&mut self, id: u64, message: &Value) -> Result<Step> {
        let kind = if self.init_id == Some(id) {
            self.init_id = None;
            "initialize"
        } else if self.thread_start_id == Some(id) {
            self.thread_start_id = None;
            "thread"
        } else if self.turn_start_id == Some(id) {
            self.turn_start_id = None;
            "turn"
        } else if self.interrupt_id == Some(id) {
            self.interrupt_id = None;
            "interrupt"
        } else {
            return Ok(Step::default());
        };
        if let Some(error) = message.get("error").filter(|e| !e.is_null()) {
            let text = error["message"].as_str().unwrap_or("unknown error");
            if kind == "initialize" {
                bail!("Codex app-server rejected the handshake: {text}");
            }
            if kind == "thread" {
                if self.options.as_ref().is_some_and(|o| o.resume.is_some()) && !self.resume_failed
                {
                    // The stored thread is gone (archived, deleted, other
                    // machine). Start a fresh thread; the caller adds the
                    // handoff context for the model.
                    self.resume_failed = true;
                    if let Some(options) = self.options.as_mut() {
                        options.resume = None;
                    }
                    let thread_id = self.id();
                    self.thread_start_id = Some(thread_id);
                    let mut params =
                        json!({"approvalPolicy":"on-request","approvalsReviewer":"user"});
                    if let Some(options) = &self.options {
                        params["cwd"] = json!(options.workspace);
                        params["sandbox"] = json!(if options.read_only {
                            "read-only"
                        } else {
                            "workspace-write"
                        });
                        if !options.model.is_empty() && options.model != "default" {
                            params["model"] = json!(options.model);
                        }
                        if let Some(rules) = options.rules_text() {
                            params["developerInstructions"] = json!(rules);
                        }
                    }
                    return Ok(Step {
                        send: vec![rpc_request(thread_id, "thread/start", params)],
                        updates: vec![Update::Warning(format!(
                            "Codex could not resume the previous thread ({text}); starting a new thread"
                        ))],
                    });
                }
                bail!("Codex app-server rejected the handshake: {text}");
            }
            if kind == "turn" {
                return Ok(Step {
                    send: self.retire_turn("Codex could not start the turn"),
                    updates: vec![Update::TurnFailed(format!(
                        "Codex could not start the turn: {text}"
                    ))],
                });
            }
            return Ok(Step::update(Update::Warning(format!(
                "Codex error: {text}"
            ))));
        }
        let result = &message["result"];
        if kind == "initialize" {
            self.phase = Phase::Initialized;
            let mut step = Step::send(rpc_notification("initialized", None));
            let thread_id = self.id();
            self.thread_start_id = Some(thread_id);
            let mut params = json!({"approvalPolicy":"on-request","approvalsReviewer":"user"});
            let mut method = "thread/start";
            if let Some(options) = &self.options {
                params["cwd"] = json!(options.workspace);
                params["sandbox"] = json!(if options.read_only {
                    "read-only"
                } else {
                    "workspace-write"
                });
                if !options.model.is_empty() && options.model != "default" {
                    params["model"] = json!(options.model);
                }
                // The user's rulebook, as Codex's own per-thread developer
                // instructions (never written to ~/.codex).
                if let Some(rules) = options.rules_text() {
                    params["developerInstructions"] = json!(rules);
                }
                if let Some(thread) = options.resume.as_deref().filter(|t| !t.is_empty()) {
                    // Documented resume: `thread/resume {threadId}` reopens the
                    // stored Codex thread; a fresh turn continues it.
                    params["threadId"] = json!(thread);
                    params["excludeTurns"] = json!(true);
                    method = "thread/resume";
                }
            }
            step.send.push(rpc_request(thread_id, method, params));
            return Ok(step);
        }
        if kind == "thread" {
            let Some(thread) = result["thread"]["id"].as_str().filter(|id| !id.is_empty()) else {
                bail!("Codex thread/start response has no thread id")
            };
            self.thread_id = Some(thread.to_owned());
            self.phase = Phase::ThreadStarted;
            let mut step = Step::update(Update::NativeSession {
                id: thread.to_owned(),
            });
            if let Some((prompt, images)) = self.pending_prompt.take() {
                step.send.extend(self.start_turn(&prompt, &images)?);
            }
            return Ok(step);
        }
        if kind == "turn" {
            let Some(turn) = result["turn"]["id"].as_str().filter(|id| !id.is_empty()) else {
                return Ok(Step {
                    send: self.retire_turn("Codex turn/start response has no valid turn id"),
                    updates: vec![Update::TurnFailed(
                        "Codex turn/start response has no valid turn id".into(),
                    )],
                });
            };
            self.turn_id = Some(turn.to_owned());
            self.early_bytes = 0;
            let frames = std::mem::take(&mut self.early_frames);
            let mut result = Step::default();
            for frame in frames {
                let step = self.dispatch_scoped(&frame);
                result.send.extend(step.send);
                result.updates.extend(step.updates);
            }
            return Ok(result);
        }
        if kind == "interrupt" {
            return Ok(Step::default());
        }
        Ok(Step::default())
    }
    fn handle_notification(&mut self, method: &str, params: &Value) -> Step {
        match method {
            "item/agentMessage/delta" => {
                if let Some(item) = params["itemId"].as_str() {
                    self.streamed_message_ids.insert(item.to_owned());
                }
                match params["delta"].as_str() {
                    Some(delta) if !delta.is_empty() => {
                        let item = params["itemId"].as_str().unwrap_or("");
                        Step::update(Update::Text(self.message_text(item, delta)))
                    }
                    _ => Step::default(),
                }
            }
            "item/started" => item_started(&params["item"]),
            "item/completed" => self.item_completed(&params["item"]),
            "turn/completed" => {
                let send = self.retire_turn("The Codex turn completed");
                let status = params["turn"]["status"].as_str().unwrap_or("completed");
                let mut step = match status {
                    "failed" if usage_limit_error(&params["turn"]["error"]) => {
                        Step::update(Update::LimitReached(
                            params["turn"]["error"]["message"]
                                .as_str()
                                .map(redact)
                                .unwrap_or_else(|| "Codex usage limit exceeded".into()),
                        ))
                    }
                    "failed" => Step::update(Update::TurnFailed(
                        params["turn"]["error"]["message"]
                            .as_str()
                            .map(redact)
                            .unwrap_or_else(|| "Codex turn failed".into()),
                    )),
                    "interrupted" => Step::update(Update::TurnCompleted {
                        text: None,
                        interrupted: true,
                    }),
                    _ => Step::update(Update::TurnCompleted {
                        text: None,
                        interrupted: false,
                    }),
                };
                step.send.extend(send);
                step
            }
            "thread/tokenUsage/updated" => {
                // `total` is cumulative for the thread; `last` is the latest
                // model call. Summing `last` per notification counts every
                // call once and never re-adds earlier turns.
                let last = &params["tokenUsage"]["last"];
                match (last["inputTokens"].as_u64(), last["outputTokens"].as_u64()) {
                    (Some(input), Some(output)) => Step::update(Update::Usage {
                        input,
                        output,
                        cached: last["cachedInputTokens"].as_u64().unwrap_or(0),
                    }),
                    _ => Step::default(),
                }
            }
            "account/rateLimits/updated" => {
                let snapshot = params["rateLimits"].clone();
                if !snapshot.is_object() {
                    return Step::default();
                }
                let mut step = Step::default();
                if let Some(kind) = snapshot["rateLimitReachedType"].as_str() {
                    step.updates.push(Update::RateLimits(snapshot.clone()));
                    step.updates.push(Update::LimitReached(format!(
                        "Codex reported {}",
                        redact(kind).replace('_', " ")
                    )));
                } else {
                    step.updates.push(Update::RateLimits(snapshot));
                }
                step
            }
            "error"
                if usage_limit_error(&params["error"])
                    && params["willRetry"].as_bool() != Some(true) =>
            {
                Step::update(Update::LimitReached(
                    params["error"]["message"]
                        .as_str()
                        .map(redact)
                        .unwrap_or_else(|| "Codex usage limit exceeded".into()),
                ))
            }
            "error" => {
                let message = params["error"]["message"]
                    .as_str()
                    .map(redact)
                    .unwrap_or_else(|| "Codex reported an error".into());
                if params["willRetry"].as_bool() == Some(true) {
                    Step::update(Update::Warning(format!("Codex will retry: {message}")))
                } else {
                    Step::update(Update::Warning(format!("Codex error: {message}")))
                }
            }
            "warning" | "configWarning" | "deprecationNotice" | "guardianWarning" => {
                let text = params["message"]
                    .as_str()
                    .or_else(|| params["summary"].as_str())
                    .unwrap_or(method);
                Step::update(Update::Warning(redact(text)))
            }
            _ => Step::default(),
        }
    }
    /// Redacted text of agent message `item`; the first text of a new
    /// message in the turn starts a new paragraph.
    fn message_text(&mut self, item: &str, text: &str) -> String {
        let text = redact(text);
        let new_message = self.last_text_item.as_deref() != Some(item);
        let paragraph = new_message && self.last_text_item.is_some();
        self.last_text_item = Some(item.to_owned());
        if paragraph && !text.starts_with('\n') {
            format!("\n\n{text}")
        } else {
            text
        }
    }
    fn item_completed(&mut self, item: &Value) -> Step {
        let id = item["id"].as_str().unwrap_or("").to_owned();
        match item["type"].as_str().unwrap_or("") {
            "agentMessage" => {
                // Text that was already streamed through deltas is not emitted
                // again; a message that arrived only as a completed item is.
                if self.streamed_message_ids.contains(&id) {
                    return Step::default();
                }
                match item["text"].as_str() {
                    Some(text) if !text.is_empty() => {
                        Step::update(Update::Text(self.message_text(&id, text)))
                    }
                    _ => Step::default(),
                }
            }
            "commandExecution" => {
                let status = item["status"].as_str().unwrap_or("");
                let exit = item["exitCode"].as_i64();
                let success = status == "completed" && exit.unwrap_or(0) == 0;
                Step::update(Update::ToolCompleted {
                    id,
                    name: "codex.command_execution".into(),
                    success,
                    output: redact_value(json!({
                        "command": item["command"],
                        "status": status,
                        "exit_code": exit,
                        "output": item["aggregatedOutput"].as_str().map(|o| clip(o, OUTPUT_PREVIEW)),
                        "duration_ms": item["durationMs"],
                    })),
                })
            }
            "fileChange" => {
                let paths: Vec<String> = item["changes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c["path"].as_str().map(str::to_owned))
                    .collect();
                let status = item["status"].as_str().unwrap_or("");
                let detail = redact_value(json!({
                    "status": status,
                    "changes": item["changes"].as_array().into_iter().flatten().map(|c| json!({
                        "path": c["path"],
                        "kind": c["kind"]["type"],
                    })).collect::<Vec<_>>(),
                }));
                Step {
                    send: Vec::new(),
                    updates: vec![
                        Update::ToolCompleted {
                            id,
                            name: "codex.file_change".into(),
                            success: status == "completed",
                            output: detail.clone(),
                        },
                        Update::FilesChanged { paths, detail },
                    ],
                }
            }
            "mcpToolCall" => Step::update(Update::ToolCompleted {
                id,
                name: format!(
                    "codex.mcp:{}/{}",
                    item["server"].as_str().unwrap_or("?"),
                    item["tool"].as_str().unwrap_or("?")
                ),
                success: item["status"].as_str() == Some("completed") && item["error"].is_null(),
                output: redact_value(
                    json!({"status":item["status"],"error":item["error"],"result":clip(&item["result"].to_string(), OUTPUT_PREVIEW)}),
                ),
            }),
            "webSearch" => Step::update(Update::ToolCompleted {
                id,
                name: "codex.web_search".into(),
                success: true,
                output: redact_value(json!({"query":item["query"]})),
            }),
            "dynamicToolCall" => Step::update(Update::ToolCompleted {
                id,
                name: format!("codex.tool:{}", item["tool"].as_str().unwrap_or("?")),
                success: item["success"]
                    .as_bool()
                    .unwrap_or(item["status"].as_str() == Some("completed")),
                output: redact_value(json!({"status":item["status"]})),
            }),
            _ => Step::default(),
        }
    }
    fn handle_server_request(&mut self, id: &Value, method: &str, params: &Value) -> Step {
        let key = request_id_key(id);
        match method {
            "item/commandExecution/requestApproval" => {
                let command = params["command"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| {
                        params["commandActions"].as_array().and_then(|actions| {
                            actions
                                .iter()
                                .find_map(|a| a["command"].as_str().map(str::to_owned))
                        })
                    })
                    .unwrap_or_else(|| "(command not provided)".into());
                self.pending_approvals.insert(key.clone(), method.into());
                Step::update(Update::Approval(ApprovalPrompt {
                    request_id: key,
                    tool_identity: params["itemId"].as_str().filter(|id| !id.is_empty()).map(super::approval_tool_identity),
                    kind: "command".into(),
                    tool: "codex.command_execution".into(),
                    command: redact(&command),
                    reason: redact(params["reason"].as_str().unwrap_or("Codex requests permission to run a shell command outside its sandbox")),
                    arguments: redact_value(json!({"command":command,"cwd":params["cwd"],"item_id":params["itemId"]})),
                }))
            }
            "item/fileChange/requestApproval" => {
                self.pending_approvals.insert(key.clone(), method.into());
                Step::update(Update::Approval(ApprovalPrompt {
                    request_id: key,
                    tool_identity: params["itemId"].as_str().filter(|id| !id.is_empty()).map(super::approval_tool_identity),
                    kind: "file_change".into(),
                    tool: "codex.file_change".into(),
                    command: format!(
                        "Apply file changes{}",
                        params["grantRoot"]
                            .as_str()
                            .map(|root| format!(" under {root}"))
                            .unwrap_or_default()
                    ),
                    reason: redact(params["reason"].as_str().unwrap_or("Codex requests permission to write files")),
                    arguments: redact_value(json!({"item_id":params["itemId"],"grant_root":params["grantRoot"]})),
                }))
            }
            "execCommandApproval" => {
                // An argv array: quoted as a shell reads it back, so
                // `["bash","-lc","git push --force"]` keeps its script as one word.
                let command = params["command"]
                    .as_array()
                    .map(|parts| {
                        crate::approvals::assess::join_argv(parts.iter().filter_map(Value::as_str))
                    })
                    .unwrap_or_default();
                self.pending_approvals.insert(key.clone(), method.into());
                Step::update(Update::Approval(ApprovalPrompt {
                    request_id: key,
                    tool_identity: None,
                    kind: "command".into(),
                    tool: "codex.command_execution".into(),
                    command: redact(&command),
                    reason: redact(params["reason"].as_str().unwrap_or("Codex requests permission to run a shell command")),
                    arguments: redact_value(json!({"command":command,"cwd":params["cwd"]})),
                }))
            }
            "applyPatchApproval" => {
                self.pending_approvals.insert(key.clone(), method.into());
                Step::update(Update::Approval(ApprovalPrompt {
                    request_id: key,
                    tool_identity: None,
                    kind: "file_change".into(),
                    tool: "codex.file_change".into(),
                    command: "Apply patch".into(),
                    reason: redact(params["reason"].as_str().unwrap_or("Codex requests permission to apply a patch")),
                    arguments: redact_value(json!({
                        "files":params["fileChanges"].as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()),
                        // Shown as a diff on the approval card.
                        "changes":params["fileChanges"],
                    })),
                }))
            }
            "item/permissions/requestApproval" => {
                self.pending_approvals.insert(key.clone(), method.into());
                Step::update(Update::Approval(ApprovalPrompt {
                    request_id: key,
                    tool_identity: None,
                    kind: "permissions".into(),
                    tool: "codex.permissions".into(),
                    command: format!(
                        "Grant additional permissions: {}",
                        clip(&params["permissions"].to_string(), 2000)
                    ),
                    reason: redact(params["reason"].as_str().unwrap_or("Codex requests extra sandbox permissions for this turn")),
                    arguments: redact_value(json!({"permissions":params["permissions"],"cwd":params["cwd"]})),
                }))
            }
            // ShadowCode holds no vendor credentials and never brokers tokens.
            "account/chatgptAuthTokens/refresh" => Step {
                send: vec![rpc_error(id, -32601, "ShadowCode does not hold or refresh ChatGPT credentials; run `codex login`")],
                updates: vec![Update::Warning(
                    "Codex asked ShadowCode to refresh its login token; refused. Run `codex login` in a terminal if the session expired.".into(),
                )],
            },
            // Free-form questions and MCP elicitations have no UI here yet.
            _ => Step {
                send: vec![rpc_error(id, -32601, "ShadowCode does not support this request; the agent should continue without it")],
                updates: vec![Update::Warning(format!(
                    "Codex request `{method}` is not supported by ShadowCode and was declined"
                ))],
            },
        }
    }
}
/// Codex `TurnError.codexErrorInfo` is `"usageLimitExceeded"` (or an object
/// keyed by it) when the plan allowance is used up.
fn usage_limit_error(error: &Value) -> bool {
    let info = &error["codexErrorInfo"];
    info.as_str() == Some("usageLimitExceeded") || info.get("usageLimitExceeded").is_some()
}

fn item_started(item: &Value) -> Step {
    let id = item["id"].as_str().unwrap_or("").to_owned();
    match item["type"].as_str().unwrap_or("") {
        "commandExecution" => Step::update(Update::ToolStarted {
            id,
            name: "codex.command_execution".into(),
            detail: redact_value(json!({"command":item["command"],"cwd":item["cwd"]})),
        }),
        "fileChange" => Step::update(Update::ToolStarted {
            id,
            name: "codex.file_change".into(),
            detail: redact_value(
                json!({"paths":item["changes"].as_array().into_iter().flatten().filter_map(|c| c["path"].as_str()).collect::<Vec<_>>()}),
            ),
        }),
        "mcpToolCall" => Step::update(Update::ToolStarted {
            id,
            name: format!(
                "codex.mcp:{}/{}",
                item["server"].as_str().unwrap_or("?"),
                item["tool"].as_str().unwrap_or("?")
            ),
            detail: redact_value(json!({"arguments":item["arguments"]})),
        }),
        "webSearch" => Step::update(Update::ToolStarted {
            id,
            name: "codex.web_search".into(),
            detail: redact_value(json!({"query":item["query"]})),
        }),
        "dynamicToolCall" => Step::update(Update::ToolStarted {
            id,
            name: format!("codex.tool:{}", item["tool"].as_str().unwrap_or("?")),
            detail: redact_value(json!({"arguments":item["arguments"]})),
        }),
        _ => Step::default(),
    }
}
impl CliAdapter for CodexAppServerAdapter {
    fn vendor(&self) -> Vendor {
        Vendor::Codex
    }
    fn command(&self, options: &LaunchOptions) -> (String, Vec<String>) {
        let mut args = effort_override(options);
        // Root `-c` overrides apply to the app-server's threads.
        args.extend(super::McpServerSpec::codex_overrides(&options.mcp_servers));
        args.push("app-server".into());
        (options.binary.clone(), args)
    }
    fn on_start(&mut self, options: &LaunchOptions) -> Vec<String> {
        self.options = Some(options.clone());
        let id = self.id();
        self.init_id = Some(id);
        vec![rpc_request(
            id,
            "initialize",
            json!({"clientInfo":{"name":"shadowcode","title":"ShadowCode","version":crate::VERSION},"capabilities":{"experimentalApi":true}}),
        )]
    }
    fn ready(&self) -> bool {
        self.phase == Phase::ThreadStarted
    }
    fn prompt(&mut self, text: &str, images: &[PromptImage]) -> Result<Vec<String>> {
        if self.ready() {
            self.start_turn(text, images)
        } else {
            self.pending_prompt = Some((text.to_owned(), images.to_vec()));
            Ok(Vec::new())
        }
    }
    fn on_line(&mut self, line: &str) -> Result<Step> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(Step::default());
        }
        let message: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => {
                return Ok(Step::update(Update::Warning(format!(
                    "Ignored a non-JSON line from codex: {}",
                    clip(&redact(trimmed), 200)
                ))))
            }
        };
        if !message.is_object() {
            return Ok(Step::update(Update::Warning(
                "Ignored a non-object JSON-RPC frame from codex".into(),
            )));
        }
        let method = message["method"].as_str();
        let id = message.get("id").filter(|id| !id.is_null());
        match (method, id) {
            (Some(_), _) => Ok(self.dispatch_scoped(&message)),
            (None, Some(id)) => match id.as_u64() {
                Some(id) => self.handle_response(id, &message),
                None => Ok(Step::update(Update::Warning(
                    "Ignored a response with an unknown request id from codex".into(),
                ))),
            },
            (None, None) => Ok(Step::update(Update::Warning(
                "Ignored a JSON-RPC frame without method or id from codex".into(),
            ))),
        }
    }
    fn approve(&mut self, request_id: &str, approve: bool) -> Result<Vec<String>> {
        self.answer(
            request_id,
            &VendorAnswer {
                allow: approve,
                ..VendorAnswer::default()
            },
        )
    }
    /// "Allow for this task" answers with Codex's own session-wide choice
    /// (`acceptForSession` / `approved_for_session`). Codex's decisions carry
    /// no reason, so a deny note is not sent.
    fn answer(&mut self, request_id: &str, answer: &VendorAnswer) -> Result<Vec<String>> {
        let Some(method) = self.pending_approvals.remove(request_id) else {
            bail!("Unknown Codex approval request {request_id}")
        };
        let (approve, session) = (answer.allow, answer.allow && answer.for_session);
        let id = request_id_value(request_id);
        let line = match method.as_str() {
            "execCommandApproval" | "applyPatchApproval" => rpc_result(
                &id,
                json!({"decision": match (approve, session) {
                    (true, true) => "approved_for_session",
                    (true, false) => "approved",
                    _ => "denied",
                }}),
            ),
            "item/permissions/requestApproval" => {
                if approve {
                    rpc_result(
                        &id,
                        json!({"permissions":{},"scope":if session {"session"} else {"turn"}}),
                    )
                } else {
                    rpc_error(
                        &id,
                        -32000,
                        "User denied the permission request in ShadowCode",
                    )
                }
            }
            _ => rpc_result(
                &id,
                json!({"decision": match (approve, session) {
                    (true, true) => "acceptForSession",
                    (true, false) => "accept",
                    _ => "decline",
                }}),
            ),
        };
        Ok(vec![line])
    }
    fn native_session(&self) -> Option<String> {
        self.thread_id.clone()
    }
    fn interrupt(&mut self) -> Vec<String> {
        let (Some(thread), Some(turn)) = (self.thread_id.clone(), self.turn_id.clone()) else {
            return Vec::new();
        };
        if !self.turn_active {
            return Vec::new();
        }
        let id = self.id();
        self.interrupt_id = Some(id);
        vec![rpc_request(
            id,
            "turn/interrupt",
            json!({"threadId":thread,"turnId":turn}),
        )]
    }
}

/// `codex exec --json` one-shot translator (fallback only). Codex runs the
/// whole turn with its configured approval policy and exits; there is no
/// approval channel, so nothing needs an answer.
#[derive(Default)]
pub struct CodexExecAdapter {
    prompt: Option<String>,
    started: bool,
    completed: bool,
    /// The rulebook, sent once ahead of the first prompt.
    rules: Option<String>,
}
impl CodexExecAdapter {
    fn with_rules(&mut self, text: String) -> String {
        match self.rules.take() {
            Some(rules) => format!("{rules}\n\n{text}"),
            None => text,
        }
    }
}
impl CliAdapter for CodexExecAdapter {
    fn vendor(&self) -> Vendor {
        Vendor::Codex
    }
    fn command(&self, options: &LaunchOptions) -> (String, Vec<String>) {
        let mut args = effort_override(options);
        args.extend(super::McpServerSpec::codex_overrides(&options.mcp_servers));
        args.extend([
            "exec".to_owned(),
            "--json".to_owned(),
            "--skip-git-repo-check".to_owned(),
            "--cd".to_owned(),
            options.workspace.display().to_string(),
        ]);
        args.push("--sandbox".into());
        args.push(if options.read_only {
            "read-only".into()
        } else {
            "workspace-write".into()
        });
        if !options.model.is_empty() && options.model != "default" {
            args.push("--model".into());
            args.push(options.model.clone());
        }
        // The prompt is read from stdin ("-"), keeping it out of `ps` output.
        args.push("-".into());
        (options.binary.clone(), args)
    }
    fn on_start(&mut self, options: &LaunchOptions) -> Vec<String> {
        self.started = true;
        self.rules = options.rules_text().map(str::to_owned);
        match self.prompt.take() {
            Some(prompt) => vec![self.with_rules(prompt)],
            None => Vec::new(),
        }
    }
    fn ready(&self) -> bool {
        self.started
    }
    fn prompt(&mut self, text: &str, images: &[PromptImage]) -> Result<Vec<String>> {
        if !images.is_empty() {
            bail!("codex exec cannot accept image bytes; use the official app-server image input");
        }
        if self.started {
            Ok(vec![self.with_rules(text.to_owned())])
        } else {
            self.prompt = Some(text.to_owned());
            Ok(Vec::new())
        }
    }
    fn on_line(&mut self, line: &str) -> Result<Step> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(Step::default());
        }
        let Ok(event) = serde_json::from_str::<Value>(trimmed) else {
            return Ok(Step::update(Update::Warning(format!(
                "Ignored a non-JSON line from codex exec: {}",
                clip(&redact(trimmed), 200)
            ))));
        };
        if !event.is_object() {
            return Ok(Step::update(Update::Warning(
                "Ignored a non-object JSONL frame from codex exec".into(),
            )));
        }
        if event.get("type").and_then(Value::as_str).is_none() {
            return Ok(Step::update(Update::Warning(
                "Ignored a JSONL frame without type from codex exec".into(),
            )));
        }
        let item = &event["item"];
        Ok(match event["type"].as_str().unwrap_or("") {
            "item.started" => match item["type"].as_str().unwrap_or("") {
                "command_execution" => Step::update(Update::ToolStarted {
                    id: item["id"].as_str().unwrap_or("").into(),
                    name: "codex.command_execution".into(),
                    detail: redact_value(json!({"command":item["command"]})),
                }),
                "file_change" => Step::update(Update::ToolStarted {
                    id: item["id"].as_str().unwrap_or("").into(),
                    name: "codex.file_change".into(),
                    detail: Value::Null,
                }),
                _ => Step::default(),
            },
            "item.completed" => match item["type"].as_str().unwrap_or("") {
                "agent_message" => match item["text"].as_str() {
                    Some(text) if !text.is_empty() => Step::update(Update::Text(redact(text))),
                    _ => Step::default(),
                },
                "command_execution" => Step::update(Update::ToolCompleted {
                    id: item["id"].as_str().unwrap_or("").into(),
                    name: "codex.command_execution".into(),
                    success: item["exit_code"].as_i64().unwrap_or(0) == 0
                        && item["status"].as_str() != Some("failed"),
                    output: redact_value(
                        json!({"command":item["command"],"exit_code":item["exit_code"],"output":item["aggregated_output"].as_str().map(|o| clip(o, OUTPUT_PREVIEW))}),
                    ),
                }),
                "file_change" => {
                    let paths: Vec<String> = item["changes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c["path"].as_str().map(str::to_owned))
                        .collect();
                    Step {
                        send: Vec::new(),
                        updates: vec![
                            Update::ToolCompleted {
                                id: item["id"].as_str().unwrap_or("").into(),
                                name: "codex.file_change".into(),
                                success: item["status"].as_str() != Some("failed"),
                                output: redact_value(json!({"changes":item["changes"]})),
                            },
                            Update::FilesChanged {
                                paths,
                                detail: redact_value(json!({"changes":item["changes"]})),
                            },
                        ],
                    }
                }
                _ => Step::default(),
            },
            "turn.completed" => {
                self.completed = true;
                let usage = &event["usage"];
                let mut step = Step::default();
                if let (Some(input), Some(output)) = (
                    usage["input_tokens"].as_u64(),
                    usage["output_tokens"].as_u64(),
                ) {
                    step.updates.push(Update::Usage {
                        input,
                        output,
                        cached: usage["cached_input_tokens"].as_u64().unwrap_or(0),
                    });
                }
                step.updates.push(Update::TurnCompleted {
                    text: None,
                    interrupted: false,
                });
                step
            }
            "turn.failed" => Step::update(Update::TurnFailed(redact(
                event["error"]["message"]
                    .as_str()
                    .unwrap_or("codex exec turn failed"),
            ))),
            "error" => Step::update(Update::TurnFailed(redact(
                event["message"].as_str().unwrap_or("codex exec error"),
            ))),
            _ => Step::default(),
        })
    }
    fn approve(&mut self, request_id: &str, _approve: bool) -> Result<Vec<String>> {
        bail!("codex exec has no approval channel (request {request_id})")
    }
    fn interrupt(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn one_shot(&self) -> bool {
        true
    }
}

//! Claude Code headless adapter.
//!
//! Spawns `claude -p --output-format stream-json --input-format stream-json
//! --verbose --include-partial-messages --permission-prompts host
//! --permission-prompt-tool stdio`
//! (https://code.claude.com/docs/en/headless). stdout is NDJSON:
//! `system/init`, `stream_event` (Anthropic streaming events, text deltas in
//! `content_block_delta`), `assistant` (complete message with `text` and
//! `tool_use` blocks), `user` (`tool_result` blocks), and a final `result`.
//! `--permission-prompts host` selects host prompts; registering the `stdio`
//! permission tool makes the CLI ask this process to answer
//! permission prompts as `control_request` / `can_use_tool` frames, answered
//! with `control_response` (`behavior: allow|deny`). Interrupts are a
//! client→CLI `control_request` with subtype `interrupt`.
//!
//! Reasoning effort is `--effort <level>`: current Claude models think
//! adaptively and ignore a thinking budget. A Claude Code without the flag
//! gets the `MAX_THINKING_TOKENS` budget instead. During a turn the CLI
//! reports the claude.ai plan windows as `rate_limit_event` frames; they
//! become the Allowance figures.
//!
//! Policy: Anthropic forbids third-party clients from using Pro/Max OAuth
//! tokens directly; driving the official `claude` binary with the user's own
//! login is currently tolerated but not guaranteed. ShadowCode never touches
//! the credential and exposes `cli_agents.claude_enabled` to opt out.
use super::{
    clip, redact, redact_value, ApprovalPrompt, CliAdapter, LaunchOptions, PromptImage, Step,
    Update, Vendor, VendorAnswer,
};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;

const OUTPUT_PREVIEW: usize = 8000;

/// Extended-thinking token budget per reasoning effort, passed as Claude
/// Code's documented `MAX_THINKING_TOKENS` environment variable to a CLI
/// that has no `--effort`. Claude Code applies it only to models without
/// adaptive thinking; on the others it merely keeps thinking on.
pub fn thinking_budget(effort: &str) -> Option<u32> {
    match effort {
        "low" => Some(4_000),
        "medium" => Some(16_000),
        "high" => Some(31_999),
        _ => None,
    }
}

#[derive(Default)]
pub struct ClaudeAdapter {
    started: bool,
    initialized: bool,
    streamed_text: bool,
    turn_text_emitted: bool,
    /// Reply text after a tool call starts a new paragraph instead of
    /// running into the text before the call.
    break_before_text: bool,
    pending_prompt: Option<(String, Vec<PromptImage>)>,
    // Keep the original input private: allow replies must echo the exact
    // requested input, while the approval card receives only its redacted copy.
    pending_permissions: HashMap<String, Value>,
    tool_names: HashMap<String, String>,
    tool_paths: HashMap<String, String>,
    turn_active: bool,
    control_counter: u64,
    session_id: Option<String>,
}
impl ClaudeAdapter {
    fn user_message(text: &str, images: &[PromptImage]) -> String {
        let mut content = vec![json!({"type":"text","text":text})];
        for image in images {
            content.push(json!({
                "type":"image",
                "source":{
                    "type":"base64",
                    "media_type":image.mime,
                    "data":image.data_base64
                }
            }));
        }
        json!({"type":"user","message":{"role":"user","content":content}}).to_string()
    }
    fn content_blocks(message: &Value) -> Vec<Value> {
        match &message["content"] {
            Value::Array(blocks) => blocks.clone(),
            Value::String(text) => vec![json!({"type":"text","text":text})],
            _ => Vec::new(),
        }
    }
    fn assistant(&mut self, message: &Value) -> Step {
        let mut step = Step::default();
        for block in Self::content_blocks(message) {
            match block["type"].as_str().unwrap_or("") {
                // Text already streamed via stream_event deltas is not
                // emitted twice; without --include-partial-messages the
                // complete message is the only copy.
                "text" if !self.streamed_text => {
                    if let Some(text) = block["text"].as_str().filter(|t| !t.is_empty()) {
                        let text = self.reply_text(text);
                        step.updates.push(Update::Text(text));
                    }
                }
                "tool_use" => {
                    self.break_before_text = self.turn_text_emitted;
                    let id = block["id"].as_str().unwrap_or("").to_owned();
                    let name = format!("claude.{}", block["name"].as_str().unwrap_or("tool"));
                    self.tool_names.insert(id.clone(), name.clone());
                    step.updates.push(Update::ToolStarted {
                        id,
                        name,
                        detail: redact_value(json!({"input":block["input"]})),
                    });
                }
                _ => {}
            }
        }
        // A new assistant message resets the streamed-text marker for the
        // next message; deltas set it again as they arrive.
        self.streamed_text = false;
        step
    }
    /// Redacted reply text; the first text after a tool call starts a new
    /// paragraph.
    fn reply_text(&mut self, text: &str) -> String {
        let paragraph = std::mem::take(&mut self.break_before_text) && self.turn_text_emitted;
        self.turn_text_emitted = true;
        let text = redact(text);
        if paragraph && !text.starts_with('\n') {
            format!("\n\n{text}")
        } else {
            text
        }
    }
    fn tool_results(&mut self, message: &Value) -> Step {
        let mut step = Step::default();
        for block in Self::content_blocks(message) {
            if block["type"] != "tool_result" {
                continue;
            }
            let id = block["tool_use_id"].as_str().unwrap_or("").to_owned();
            let name = self
                .tool_names
                .get(&id)
                .cloned()
                .unwrap_or_else(|| "claude.tool".into());
            let success = block["is_error"].as_bool() != Some(true);
            let text = match &block["content"] {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            step.updates.push(Update::ToolCompleted {
                id: id.clone(),
                name: name.clone(),
                success,
                output: redact_value(json!({"output":clip(&text, OUTPUT_PREVIEW)})),
            });
            if success {
                if let Some(paths) = self.edited_paths(&id) {
                    step.updates.push(Update::FilesChanged {
                        paths,
                        detail: json!({"tool":name}),
                    });
                }
            }
        }
        step
    }
    fn edited_paths(&self, tool_use_id: &str) -> Option<Vec<String>> {
        let name = self.tool_names.get(tool_use_id)?;
        let path = self.tool_inputs_path(tool_use_id)?;
        matches!(
            name.as_str(),
            "claude.Edit" | "claude.Write" | "claude.MultiEdit" | "claude.NotebookEdit"
        )
        .then_some(vec![path])
    }
    fn tool_inputs_path(&self, tool_use_id: &str) -> Option<String> {
        self.tool_paths.get(tool_use_id).cloned()
    }
    fn control_request(&mut self, message: &Value) -> Step {
        let request_id = message["request_id"].as_str().unwrap_or("").to_owned();
        let request = &message["request"];
        match request["subtype"].as_str().unwrap_or("") {
            "can_use_tool" => {
                let tool = request["tool_name"].as_str().unwrap_or("tool").to_owned();
                let input = &request["input"];
                let command = input["command"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| input["file_path"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| tool.clone());
                let kind = match tool.as_str() {
                    "Bash" => "command",
                    "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => "file_change",
                    _ => "tool",
                };
                if request_id.is_empty() {
                    return Step::update(Update::Warning(
                        "Claude permission request had no request_id; ignored".into(),
                    ));
                }
                self.pending_permissions
                    .insert(request_id.clone(), input.clone());
                Step::update(Update::Approval(ApprovalPrompt {
                    request_id,
                    tool_identity: request["tool_use_id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .map(super::approval_tool_identity),
                    kind: kind.into(),
                    tool: format!("claude.{tool}"),
                    command: redact(&command),
                    reason: redact(
                        request["description"]
                            .as_str()
                            .or_else(|| input["description"].as_str())
                            .unwrap_or("Claude requests permission to use a tool"),
                    ),
                    arguments: redact_value(json!({"tool":tool,"input":input})),
                }))
            }
            other => Step {
                send: if request_id.is_empty() {
                    Vec::new()
                } else {
                    vec![json!({"type":"control_response","response":{"subtype":"error","request_id":request_id,"error":"Unsupported control request"}}).to_string()]
                },
                updates: vec![Update::Warning(format!(
                    "Claude control request `{other}` is not supported and was declined"
                ))],
            },
        }
    }
}
impl CliAdapter for ClaudeAdapter {
    fn vendor(&self) -> Vendor {
        Vendor::Claude
    }
    fn command(&self, options: &LaunchOptions) -> (String, Vec<String>) {
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompts",
            "host",
            // Anthropic's SDK registers this handler for can_use_tool. The
            // host selector alone leaves prompts without a stdio responder.
            "--permission-prompt-tool",
            "stdio",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if options.read_only {
            args.push("--permission-mode".into());
            args.push("plan".into());
        }
        if !options.model.is_empty() && options.model != "default" {
            args.push("--model".into());
            args.push(options.model.clone());
        }
        // `--effort <level>` (low, medium, high, xhigh, max). Models that
        // take no effort (Haiku) ignore it.
        if let Some(effort) = options
            .effort
            .as_deref()
            .filter(|e| matches!(*e, "low" | "medium" | "high"))
            .filter(|_| !options.legacy_effort)
        {
            args.push("--effort".into());
            args.push(effort.to_owned());
        }
        if let Some(session) = options.resume.as_deref().filter(|s| !s.is_empty()) {
            // Documented: `--resume <session-id>` continues the stored
            // conversation from any directory on this machine.
            args.push("--resume".into());
            args.push(session.to_owned());
        }
        // Enabled project MCP servers, added to the user's own for this run.
        if let Some(config) = super::McpServerSpec::claude_config(&options.mcp_servers) {
            args.push("--mcp-config".into());
            args.push(config);
        }
        (options.binary.clone(), args)
    }
    fn on_start(&mut self, options: &LaunchOptions) -> Vec<String> {
        self.session_id = options.resume.clone().filter(|s| !s.is_empty());
        self.started = true;
        // stream-json input accepts the first user message immediately; the
        // `system/init` frame confirms the session started.
        match self.pending_prompt.take() {
            Some((prompt, images)) => {
                self.turn_active = true;
                vec![Self::user_message(&prompt, &images)]
            }
            None => Vec::new(),
        }
    }
    fn ready(&self) -> bool {
        self.started
    }
    fn prompt(&mut self, text: &str, images: &[PromptImage]) -> Result<Vec<String>> {
        self.streamed_text = false;
        self.turn_text_emitted = false;
        self.break_before_text = false;
        if self.started {
            self.turn_active = true;
            Ok(vec![Self::user_message(text, images)])
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
                    "Ignored a non-JSON line from claude: {}",
                    clip(&redact(trimmed), 200)
                ))))
            }
        };
        if !message.is_object() {
            return Ok(Step::update(Update::Warning(
                "Ignored a non-object frame from claude".into(),
            )));
        }
        Ok(match message["type"].as_str().unwrap_or("") {
            "system" => {
                if message["subtype"] == "init" {
                    self.initialized = true;
                }
                match message["session_id"].as_str() {
                    Some(id) if self.session_id.as_deref() != Some(id) => {
                        self.session_id = Some(id.to_owned());
                        Step::update(Update::NativeSession { id: id.to_owned() })
                    }
                    _ => Step::default(),
                }
            }
            "stream_event" => {
                let event = &message["event"];
                if event["type"] == "content_block_delta" && event["delta"]["type"] == "text_delta"
                {
                    match event["delta"]["text"].as_str() {
                        Some(text) if !text.is_empty() => {
                            self.streamed_text = true;
                            Step::update(Update::Text(self.reply_text(text)))
                        }
                        _ => Step::default(),
                    }
                } else {
                    Step::default()
                }
            }
            "assistant" => {
                let message = message["message"].clone();
                for block in Self::content_blocks(&message) {
                    if block["type"] == "tool_use" {
                        if let (Some(id), Some(path)) = (
                            block["id"].as_str(),
                            block["input"]["file_path"]
                                .as_str()
                                .or_else(|| block["input"]["notebook_path"].as_str()),
                        ) {
                            self.tool_paths.insert(id.to_owned(), path.to_owned());
                        }
                    }
                }
                self.assistant(&message)
            }
            "user" => self.tool_results(&message["message"].clone()),
            "control_request" => self.control_request(&message),
            "control_response" | "control_cancel_request" | "keep_alive" => Step::default(),
            // claude.ai plan windows, read by the CLI from the API's
            // rate-limit headers (`status`: allowed, allowed_warning,
            // rejected). Only subscription logins report them.
            "rate_limit_event" => {
                let info = &message["rate_limit_info"];
                if !info.is_object() {
                    return Ok(Step::default());
                }
                let mut step = Step::update(Update::RateLimits(info.clone()));
                if info["status"] == "rejected" && info["isUsingOverage"] != true {
                    step.updates.push(Update::LimitReached(format!(
                        "Claude reported its {} limit",
                        super::usage::claude_window_label(
                            info["rateLimitType"].as_str().unwrap_or("")
                        )
                        .to_lowercase()
                    )));
                }
                step
            }
            "result" => {
                self.turn_active = false;
                let mut step = Step::default();
                let usage = &message["usage"];
                if let (Some(input), Some(output)) = (
                    usage["input_tokens"].as_u64(),
                    usage["output_tokens"].as_u64(),
                ) {
                    // Claude counts cache reads and writes apart from
                    // `input_tokens`; ShadowCode's input includes them.
                    let cached = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    let written = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                    step.updates.push(Update::Usage {
                        input: input.saturating_add(cached).saturating_add(written),
                        output,
                        cached,
                    });
                }
                if let Some(total_usd) = message["total_cost_usd"]
                    .as_f64()
                    .filter(|c| c.is_finite() && *c >= 0.0)
                {
                    step.updates.push(Update::VendorCost { total_usd });
                }
                if message["is_error"].as_bool() == Some(true)
                    || message["subtype"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("error"))
                {
                    let detail = message["result"]
                        .as_str()
                        .or_else(|| message["error"].as_str())
                        .unwrap_or("Claude reported an error");
                    step.updates.push(Update::TurnFailed(format!(
                        "Claude {}: {}",
                        message["subtype"].as_str().unwrap_or("error"),
                        redact(detail)
                    )));
                } else {
                    // `result` repeats the final assistant text; it is only
                    // emitted when neither deltas nor complete assistant
                    // messages emitted text during this turn. The per-message
                    // streamed_text marker resets at each assistant frame.
                    let text = message["result"]
                        .as_str()
                        .filter(|t| !t.is_empty() && !self.turn_text_emitted)
                        .map(redact);
                    step.updates.push(Update::TurnCompleted {
                        text,
                        interrupted: false,
                    });
                }
                step
            }
            "" => Step::update(Update::Warning(
                "Ignored a frame without type from claude".into(),
            )),
            _ => Step::default(),
        })
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
    /// A deny note becomes the denial message Claude reads. "Allow for this
    /// task" is kept by ShadowCode (it answers later matching prompts), so
    /// no permission rule is written into Claude's settings.
    fn answer(&mut self, request_id: &str, answer: &VendorAnswer) -> Result<Vec<String>> {
        let Some(input) = self.pending_permissions.remove(request_id) else {
            bail!("Unknown Claude permission request {request_id}")
        };
        let response = if answer.allow {
            // Match the SDK permission result, without updatedPermissions:
            // this answer authorizes only the requested tool invocation.
            json!({"behavior":"allow","updatedInput":input})
        } else {
            let message = match answer.note.as_deref() {
                Some(note) => format!("The user denied this action in ShadowCode and said: {note}"),
                None => "The user denied this action in ShadowCode".into(),
            };
            json!({"behavior":"deny","message":message})
        };
        Ok(vec![json!({
            "type":"control_response",
            "response":{"subtype":"success","request_id":request_id,"response":response}
        })
        .to_string()])
    }
    fn deny_note(&self) -> bool {
        true
    }
    fn env(&self, options: &LaunchOptions) -> Vec<(String, String)> {
        options
            .effort
            .as_deref()
            .filter(|_| options.legacy_effort)
            .and_then(thinking_budget)
            .map(|budget| vec![("MAX_THINKING_TOKENS".to_owned(), budget.to_string())])
            .unwrap_or_default()
    }
    fn native_session(&self) -> Option<String> {
        self.session_id.clone()
    }
    fn interrupt(&mut self) -> Vec<String> {
        if !self.turn_active {
            return Vec::new();
        }
        self.control_counter += 1;
        vec![json!({
            "type":"control_request",
            "request_id":format!("shadowcode-interrupt-{}", self.control_counter),
            "request":{"subtype":"interrupt"}
        })
        .to_string()]
    }
}

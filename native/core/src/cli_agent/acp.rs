//! Agent Client Protocol (ACP) adapter for Cursor (`cursor-agent acp`), Grok
//! (`grok agent stdio`) and Antigravity (Google's `agy_acp_server.par`).
//!
//! ACP is JSON-RPC 2.0 over stdio (https://agentclientprotocol.com):
//! client → `initialize`, `session/new`, `session/prompt`, `session/cancel`
//! (notification); agent → `session/update` notifications whose
//! `update.sessionUpdate` is one of `agent_message_chunk`,
//! `agent_thought_chunk`, `tool_call`, `tool_call_update`, `plan`, …; and the
//! agent → client request `session/request_permission` answered with
//! `{"outcome":{"outcome":"selected","optionId":…}}`. ShadowCode declares no
//! `fs`/`terminal` client capabilities, so the vendor agent performs its own
//! file and shell work with its own sandbox.
use super::{
    clip, redact, redact_value, ApprovalPrompt, CliAdapter, LaunchOptions, PromptImage, Step,
    Update, Vendor,
};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;

const OUTPUT_PREVIEW: usize = 8000;

/// Shown when the Antigravity server has no valid Google sign-in.
pub const ANTIGRAVITY_SIGN_IN: &str = "Antigravity isn't signed in, or its sign-in expired. Choose Connect for Antigravity in Settings › Accounts.";

pub(crate) fn request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string()
}
pub(crate) fn notification(method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","method":method,"params":params}).to_string()
}
pub(crate) fn result(id: &Value, result: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"result":result}).to_string()
}
fn error(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}).to_string()
}

struct PendingPermission {
    allow: Option<String>,
    reject: Option<String>,
}

#[derive(Debug, Default, PartialEq)]
enum Phase {
    #[default]
    Starting,
    Initialized,
    Session,
}

pub struct AcpAdapter {
    vendor: Vendor,
    next_id: u64,
    phase: Phase,
    init_id: Option<u64>,
    auth_id: Option<u64>,
    session_load_id: Option<u64>,
    session_new_id: Option<u64>,
    set_model_id: Option<u64>,
    prompt_id: Option<u64>,
    session_id: Option<String>,
    pending_prompt: Option<(String, Vec<PromptImage>)>,
    pending_permissions: HashMap<String, PendingPermission>,
    tool_names: HashMap<String, String>,
    options: Option<LaunchOptions>,
    prompt_active: bool,
    /// A model switch was sent and not yet answered. Prompts wait for it:
    /// Antigravity ends a prompt at once, with no output, when the model
    /// changes underneath it.
    switching: bool,
    /// The agent accepts HTTP MCP servers (`mcpCapabilities.http`).
    mcp_http: bool,
    /// Negotiated per process, never inferred from a vendor name or cache.
    images_supported: bool,
}
impl AcpAdapter {
    pub fn new(vendor: Vendor) -> Self {
        Self {
            vendor,
            next_id: 0,
            phase: Phase::Starting,
            init_id: None,
            auth_id: None,
            session_load_id: None,
            session_new_id: None,
            set_model_id: None,
            prompt_id: None,
            session_id: None,
            pending_prompt: None,
            pending_permissions: HashMap::new(),
            tool_names: HashMap::new(),
            options: None,
            prompt_active: false,
            switching: false,
            mcp_http: false,
            images_supported: false,
        }
    }
    fn check_images(&self, images: &[PromptImage]) -> Result<()> {
        if !images.is_empty() && !self.images_supported {
            bail!("{} runtime did not advertise image support. Remove the attachment or explicitly choose an image-capable model. No image prompt was sent.", self.vendor.product_label());
        }
        Ok(())
    }
    /// The project's enabled MCP servers in ACP form.
    fn mcp_servers(&self) -> Value {
        json!(self
            .options
            .as_ref()
            .map(|o| o
                .mcp_servers
                .iter()
                .filter_map(|s| s.acp(self.mcp_http))
                .collect::<Vec<_>>())
            .unwrap_or_default())
    }
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
    fn start_prompt(&mut self, text: &str, images: &[PromptImage]) -> Result<Vec<String>> {
        self.check_images(images)?;
        let Some(session) = self.session_id.clone() else {
            bail!("ACP session is not ready")
        };
        let id = self.id();
        self.prompt_id = Some(id);
        self.prompt_active = true;
        let mut prompt = vec![json!({"type":"text","text":text})];
        for image in images {
            prompt.push(json!({
                "type":"image",
                "mimeType":image.mime,
                "data":image.data_base64
            }));
        }
        Ok(vec![request(
            id,
            "session/prompt",
            json!({"sessionId":session,"prompt":prompt}),
        )])
    }
    /// A `session/load` was sent and has not been answered yet.
    fn replaying(&self) -> bool {
        self.session_load_id.is_some() && self.phase != Phase::Session
    }
    fn cwd(&self) -> String {
        self.options
            .as_ref()
            .map(|o| o.workspace.display().to_string())
            .unwrap_or_default()
    }
    fn open_session_request(&mut self) -> String {
        if let Some(resume) = self
            .options
            .as_ref()
            .and_then(|o| o.resume.clone())
            .filter(|r| !r.is_empty())
        {
            let id = self.id();
            self.session_load_id = Some(id);
            return request(
                id,
                "session/load",
                json!({"sessionId": resume, "cwd": self.cwd(), "mcpServers": self.mcp_servers()}),
            );
        }
        self.new_session_request()
    }
    fn new_session_request(&mut self) -> String {
        let id = self.id();
        self.session_new_id = Some(id);
        request(
            id,
            "session/new",
            json!({"cwd": self.cwd(), "mcpServers": self.mcp_servers()}),
        )
    }
    /// Steps after a session exists: plan mode for read-only tasks, an exact
    /// model id when the picker chose one, then the queued prompt.
    fn after_session(&mut self, session: &str, modes: &Value) -> Result<Step> {
        let mut step = Step::update(Update::NativeSession {
            id: session.to_owned(),
        });
        if let Some(options) = self.options.clone() {
            if options.read_only
                && modes["availableModes"]
                    .as_array()
                    .is_some_and(|modes| modes.iter().any(|m| m["id"] == "plan"))
            {
                let mode_id = self.id();
                step.send.push(request(
                    mode_id,
                    "session/set_mode",
                    json!({"sessionId":session,"modeId":"plan"}),
                ));
            }
            // Antigravity lists its models as the `model` config option and
            // switches with `session/set_config_option`.
            if self.vendor == Vendor::Antigravity
                && !options.model.is_empty()
                && options.model != "default"
                && options.model != "auto"
            {
                let set_id = self.id();
                self.set_model_id = Some(set_id);
                self.switching = true;
                step.send.push(request(
                    set_id,
                    "session/set_config_option",
                    json!({"sessionId":session,"configId":"model","value":options.model}),
                ));
            }
            // Cursor ignores `--model` in ACP mode for parameterised ids and
            // only accepts the exact ids it listed in `session/new`.
            if self.vendor == Vendor::Cursor
                && !options.model.is_empty()
                && options.model != "default"
                && options.model != "auto"
            {
                let set_id = self.id();
                self.set_model_id = Some(set_id);
                self.switching = true;
                step.send.push(request(
                    set_id,
                    "session/set_model",
                    json!({"sessionId":session,"modelId":options.model}),
                ));
            }
        }
        if !self.switching {
            if let Some((prompt, images)) = self.pending_prompt.take() {
                step.send.extend(self.start_prompt(&prompt, &images)?);
            }
        }
        Ok(step)
    }
    fn handle_response(&mut self, id: u64, message: &Value) -> Result<Step> {
        if let Some(err) = message.get("error").filter(|e| !e.is_null()) {
            let text = err["data"]["message"]
                .as_str()
                .or_else(|| err["message"].as_str())
                .unwrap_or("unknown error");
            if Some(id) == self.session_load_id {
                // The stored session is gone; start a new one and let the
                // caller supply handoff context.
                let line = self.new_session_request();
                return Ok(Step {
                    send: vec![line],
                    updates: vec![Update::Warning(format!(
                        "{} could not resume the previous session ({text}); starting a new one",
                        self.vendor.product_label()
                    ))],
                });
            }
            if Some(id) == self.auth_id {
                if self.vendor == Vendor::Antigravity {
                    bail!("{}", ANTIGRAVITY_SIGN_IN);
                }
                bail!(
                    "{} rejected the login ({text}). Run `{} login` and try again.",
                    self.vendor.product_label(),
                    self.vendor.binary()
                );
            }
            if Some(id) == self.set_model_id {
                self.switching = false;
                self.pending_prompt = None;
                return Ok(Step::update(Update::TurnFailed(format!(
                    "{} does not accept model `{}` ({text}). Pick the model again from the list.",
                    self.vendor.product_label(),
                    self.options
                        .as_ref()
                        .map(|o| o.model.as_str())
                        .unwrap_or("")
                ))));
            }
            if self.vendor == Vendor::Antigravity
                && Some(id) == self.session_new_id
                && err["code"].as_i64() == Some(-32000)
            {
                bail!("{}", ANTIGRAVITY_SIGN_IN);
            }
            if Some(id) == self.init_id || Some(id) == self.session_new_id {
                bail!("{} rejected the ACP handshake: {text}", self.vendor.id());
            }
            if Some(id) == self.prompt_id {
                self.prompt_active = false;
                return Ok(Step::update(Update::TurnFailed(format!(
                    "{} could not run the prompt: {text}",
                    self.vendor.id()
                ))));
            }
            return Ok(Step::update(Update::Warning(format!(
                "{} error: {text}",
                self.vendor.id()
            ))));
        }
        let res = &message["result"];
        if Some(id) == self.init_id {
            if res["protocolVersion"].as_u64() != Some(1) {
                bail!("{} runtime returned an unsupported or missing ACP protocol version; ShadowCode supports version 1. No session was started.", self.vendor.product_label());
            }
            self.images_supported = res["agentCapabilities"]["promptCapabilities"]["image"] == true;
            if let Some((_, images)) = &self.pending_prompt {
                self.check_images(images)?;
            }
            self.phase = Phase::Initialized;
            self.mcp_http = res["agentCapabilities"]["mcpCapabilities"]["http"] == true;
            // Documented Cursor flow: `authenticate {methodId:"cursor_login"}`
            // before a session, using the login the CLI already holds.
            let methods: Vec<String> = res["authMethods"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m["id"].as_str().map(str::to_owned))
                .collect();
            let wanted = if self.vendor == Vendor::Antigravity {
                super::antigravity_server::AUTH_METHOD
            } else {
                "cursor_login"
            };
            if let Some(method) = methods.iter().find(|m| m.as_str() == wanted).cloned() {
                let auth_id = self.id();
                self.auth_id = Some(auth_id);
                return Ok(Step::send(request(
                    auth_id,
                    "authenticate",
                    json!({"methodId": method}),
                )));
            }
            return Ok(Step::send(self.open_session_request()));
        }
        if Some(id) == self.auth_id {
            return Ok(Step::send(self.open_session_request()));
        }
        if Some(id) == self.session_load_id {
            // `session/load` replays history through notifications and
            // returns an empty result; the session id is the one we asked for.
            let session = self
                .options
                .as_ref()
                .and_then(|o| o.resume.clone())
                .unwrap_or_default();
            self.session_id = Some(session.clone());
            self.phase = Phase::Session;
            return self.after_session(&session, &res["modes"]);
        }
        if Some(id) == self.session_new_id {
            let Some(session) = res["sessionId"].as_str() else {
                bail!("ACP session/new response has no sessionId")
            };
            let session = session.to_owned();
            self.session_id = Some(session.clone());
            self.phase = Phase::Session;
            return self.after_session(&session, &res["modes"]);
        }
        if Some(id) == self.set_model_id {
            // The model is in place; now send the queued prompt.
            self.switching = false;
            let mut step = Step::default();
            if let Some((prompt, images)) = self.pending_prompt.take() {
                step.send.extend(self.start_prompt(&prompt, &images)?);
            }
            return Ok(step);
        }
        if Some(id) == self.prompt_id {
            self.prompt_active = false;
            let stop = res["stopReason"].as_str().unwrap_or("end_turn");
            return Ok(match stop {
                "cancelled" => Step::update(Update::TurnCompleted {
                    text: None,
                    interrupted: true,
                }),
                "refusal" => Step::update(Update::TurnFailed(format!(
                    "{} refused the request",
                    self.vendor.id()
                ))),
                "max_tokens" | "max_turn_requests" => Step {
                    send: Vec::new(),
                    updates: vec![
                        Update::Warning(format!(
                            "{} stopped early ({stop}); the result may be incomplete",
                            self.vendor.id()
                        )),
                        Update::TurnCompleted {
                            text: None,
                            interrupted: false,
                        },
                    ],
                },
                _ => Step::update(Update::TurnCompleted {
                    text: None,
                    interrupted: false,
                }),
            });
        }
        Ok(Step::default())
    }
    fn session_update(&mut self, update: &Value) -> Step {
        match update["sessionUpdate"].as_str().unwrap_or("") {
            "agent_message_chunk" => match update["content"]["text"].as_str() {
                Some(text) if !text.is_empty() => Step::update(Update::Text(redact(text))),
                _ => Step::default(),
            },
            "tool_call" => {
                let id = update["toolCallId"].as_str().unwrap_or("").to_owned();
                let name = format!(
                    "{}.{}",
                    self.vendor.id(),
                    update["kind"].as_str().unwrap_or("tool")
                );
                self.tool_names.insert(id.clone(), name.clone());
                let mut step = Step::update(Update::ToolStarted {
                    id: id.clone(),
                    name: name.clone(),
                    detail: redact_value(
                        json!({"title":update["title"],"locations":update["locations"],"input":update["rawInput"]}),
                    ),
                });
                // Some agents report a tool as already finished in its first
                // notification.
                if matches!(
                    update["status"].as_str(),
                    Some("completed") | Some("failed")
                ) {
                    step = step.merge(self.tool_finished(&id, &name, update));
                }
                step
            }
            "tool_call_update" => {
                let id = update["toolCallId"].as_str().unwrap_or("").to_owned();
                let name = self
                    .tool_names
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| format!("{}.tool", self.vendor.id()));
                match update["status"].as_str() {
                    Some("completed") | Some("failed") => self.tool_finished(&id, &name, update),
                    _ => Step::default(),
                }
            }
            "usage_update" => {
                match (
                    update["inputTokens"].as_u64(),
                    update["outputTokens"].as_u64(),
                ) {
                    (Some(input), Some(output)) => Step::update(Update::Usage {
                        input,
                        output,
                        cached: update["cachedReadTokens"].as_u64().unwrap_or(0),
                    }),
                    _ => Step::default(),
                }
            }
            _ => Step::default(),
        }
    }
    fn tool_finished(&mut self, id: &str, name: &str, update: &Value) -> Step {
        let success = update["status"].as_str() == Some("completed");
        let paths: Vec<String> = update["locations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["path"].as_str().map(str::to_owned))
            .collect();
        let mut content = Vec::new();
        for block in update["content"].as_array().into_iter().flatten() {
            if let Some(text) = block["content"]["text"].as_str() {
                content.push(clip(text, OUTPUT_PREVIEW).to_owned());
            } else if block["type"] == "diff" {
                if let Some(path) = block["path"].as_str() {
                    content.push(format!("diff {path}"));
                }
            }
        }
        let output = redact_value(json!({
            "status": update["status"],
            "title": update["title"],
            "locations": paths,
            "content": content,
        }));
        let mut step = Step::update(Update::ToolCompleted {
            id: id.to_owned(),
            name: name.to_owned(),
            success,
            output: output.clone(),
        });
        let edits = name.ends_with(".edit") || name.ends_with(".delete") || name.ends_with(".move");
        if success && edits && !paths.is_empty() {
            step.updates.push(Update::FilesChanged {
                paths,
                detail: output,
            });
        }
        step
    }
    fn permission_request(&mut self, id: &Value, params: &Value) -> Step {
        // Antigravity sends questions for the user through the permission
        // method with an `interaction_` tool call id; their options are
        // answers, not approvals. ShadowCode can't show them yet.
        if self.vendor == Vendor::Antigravity
            && params["toolCall"]["toolCallId"]
                .as_str()
                .is_some_and(|t| t.starts_with("interaction_"))
        {
            let question = params["toolCall"]["title"].as_str().unwrap_or("a question");
            return Step {
                send: vec![result(id, json!({"outcome":{"outcome":"cancelled"}}))],
                updates: vec![Update::Warning(format!(
                    "Antigravity asked \"{}\"; ShadowCode can't answer agent questions yet, so it was skipped. Put the answer in your next message.",
                    clip(&redact(question), 200)
                ))],
            };
        }
        let key = id.to_string();
        let mut allow = None;
        let mut reject = None;
        for option in params["options"].as_array().into_iter().flatten() {
            let option_id = option["optionId"].as_str().map(str::to_owned);
            match option["kind"].as_str() {
                Some("allow_once") => allow = option_id.or(allow),
                Some("allow_always") if allow.is_none() => allow = option_id,
                Some("reject_once") => reject = option_id.or(reject),
                Some("reject_always") if reject.is_none() => reject = option_id,
                _ => {}
            }
        }
        if allow.is_none() && reject.is_none() {
            return Step {
                send: vec![error(
                    id,
                    -32602,
                    "No usable permission options were offered",
                )],
                updates: vec![Update::Warning(
                    "Permission request offered no allow/reject options; declined".into(),
                )],
            };
        }
        self.pending_permissions
            .insert(key.clone(), PendingPermission { allow, reject });
        let tool = &params["toolCall"];
        let title = tool["title"].as_str().unwrap_or("tool call");
        let kind = tool["kind"].as_str().unwrap_or("tool");
        let command = tool["rawInput"]["command"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| title.to_owned());
        Step::update(Update::Approval(ApprovalPrompt {
            request_id: key,
            kind: match kind {
                "execute" => "command".into(),
                "edit" | "delete" | "move" => "file_change".into(),
                _ => "tool".into(),
            },
            tool: format!("{}.{kind}", self.vendor.id()),
            command: redact(&command),
            reason: redact(&format!(
                "{} requests permission: {title}",
                self.vendor.label()
            )),
            arguments: redact_value(
                // `content` carries the proposed edit as ACP diff blocks,
                // shown on the approval card.
                json!({"tool_call_id":tool["toolCallId"],"title":title,"input":tool["rawInput"],"locations":tool["locations"],"content":tool["content"]}),
            ),
        }))
    }
}
impl CliAdapter for AcpAdapter {
    fn vendor(&self) -> Vendor {
        self.vendor
    }
    fn command(&self, options: &LaunchOptions) -> (String, Vec<String>) {
        let mut args = Vec::new();
        match self.vendor {
            // Official Cursor ACP: `cursor-agent acp`
            // (https://cursor.com/docs/cli/acp). The model is selected per
            // session with `session/set_model`; the `--model` flag would also
            // rewrite the user's CLI default.
            Vendor::Cursor => args.push("acp".into()),
            // Google's ACP server; the model is chosen per session.
            Vendor::Antigravity => args = super::antigravity_server::launch_args(),
            _ => {
                args.push("agent".into());
                if !options.model.is_empty()
                    && options.model != "default"
                    && options.model != "auto"
                {
                    args.push("--model".into());
                    args.push(options.model.clone());
                }
                args.push("stdio".into());
            }
        }
        (options.binary.clone(), args)
    }
    fn native_session(&self) -> Option<String> {
        self.session_id.clone()
    }
    fn on_start(&mut self, options: &LaunchOptions) -> Vec<String> {
        self.options = Some(options.clone());
        self.images_supported = false;
        let id = self.id();
        self.init_id = Some(id);
        vec![request(
            id,
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
                "clientInfo": {"name": "shadowcode", "title": "ShadowCode", "version": crate::VERSION}
            }),
        )]
    }
    fn ready(&self) -> bool {
        self.phase == Phase::Session && !self.switching
    }
    fn prompt(&mut self, text: &str, images: &[PromptImage]) -> Result<Vec<String>> {
        if self.ready() {
            self.start_prompt(text, images)
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
                    "Ignored a non-JSON line from {}: {}",
                    self.vendor.id(),
                    clip(&redact(trimmed), 200)
                ))))
            }
        };
        if !message.is_object() {
            return Ok(Step::update(Update::Warning(format!(
                "Ignored a non-object frame from {}",
                self.vendor.id()
            ))));
        }
        let method = message["method"].as_str();
        let id = message.get("id").filter(|id| !id.is_null());
        match (method, id) {
            (Some("session/request_permission"), Some(id)) => {
                Ok(self.permission_request(id, &message["params"]))
            }
            (Some("fs/read_text_file"), Some(id))
            | (Some("fs/write_text_file"), Some(id))
            | (Some("terminal/create"), Some(id)) => Ok(Step {
                send: vec![error(id, -32601, "ShadowCode declared no fs/terminal capabilities; the agent performs its own I/O")],
                updates: vec![Update::Warning(format!(
                    "{} asked ShadowCode to perform `{}`; declined (vendor agent owns its tools)",
                    self.vendor.id(),
                    method.unwrap_or("")
                ))],
            }),
            (Some(other), Some(id)) => Ok(Step {
                send: vec![error(id, -32601, "Method not supported by ShadowCode")],
                updates: vec![Update::Warning(format!(
                    "{} request `{other}` is not supported and was declined",
                    self.vendor.id()
                ))],
            }),
            // `session/load` replays the stored conversation as updates
            // before it answers; that history was already shown and must not
            // be counted as new output of this turn.
            (Some("session/update"), None) if self.replaying() => Ok(Step::default()),
            (Some("session/update"), None) => Ok(self.session_update(&message["params"]["update"])),
            (Some(_), None) => Ok(Step::default()),
            (None, Some(id)) => match id.as_u64() {
                Some(id) => self.handle_response(id, &message),
                None => Ok(Step::update(Update::Warning(format!(
                    "Ignored a response with an unknown id from {}",
                    self.vendor.id()
                )))),
            },
            (None, None) => Ok(Step::update(Update::Warning(format!(
                "Ignored a frame without method or id from {}",
                self.vendor.id()
            )))),
        }
    }
    fn approve(&mut self, request_id: &str, approve: bool) -> Result<Vec<String>> {
        let Some(pending) = self.pending_permissions.remove(request_id) else {
            bail!("Unknown ACP permission request {request_id}")
        };
        let id: Value = serde_json::from_str(request_id)
            .unwrap_or_else(|_| Value::String(request_id.to_owned()));
        let choice = if approve {
            pending.allow
        } else {
            pending.reject
        };
        Ok(vec![match choice {
            Some(option) => result(
                &id,
                json!({"outcome":{"outcome":"selected","optionId":option}}),
            ),
            // The agent offered no option matching the decision; a cancelled
            // outcome is the protocol's safe refusal.
            None => result(&id, json!({"outcome":{"outcome":"cancelled"}})),
        }])
    }
    fn interrupt(&mut self) -> Vec<String> {
        match &self.session_id {
            Some(session) if self.prompt_active => {
                vec![notification("session/cancel", json!({"sessionId":session}))]
            }
            _ => Vec::new(),
        }
    }
}

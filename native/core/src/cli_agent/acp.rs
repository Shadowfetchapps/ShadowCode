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
const TOOL_METADATA_COUNT: usize = 128;
const TOOL_METADATA_BYTES: usize = 64_000;
const RAW_OUTPUT_BYTES: usize = 8000;

#[derive(Clone)]
struct RawToolOutput {
    value: Value,
    truncated: bool,
}

impl RawToolOutput {
    fn capture(value: &Value) -> Self {
        // Redact before clipping, including known credential keys: clipping
        // first could leave a token prefix that no longer matches redaction.
        let mut value = value.clone();
        crate::redaction::redact_known_secrets(&mut value);
        let value = redact_value(value);
        let encoded = value.to_string();
        if encoded.len() <= RAW_OUTPUT_BYTES {
            return Self {
                value,
                truncated: false,
            };
        }
        // Oversize JSON becomes explicitly labeled JSON text, not a partial
        // structured result. Each source byte needs at most two bytes when
        // this already-escaped JSON prefix is serialized as a JSON string.
        let mut end = (RAW_OUTPUT_BYTES - 2) / 2;
        while !encoded.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            value: Value::String(encoded[..end].to_owned()),
            truncated: true,
        }
    }
}

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
    // ACP permission requests are partial tool updates. Keep a bounded view
    // of current pending tools so omitted input cannot erase the operation
    // the user is being asked to approve.
    tool_metadata: HashMap<String, Value>,
    // Kept separately: a large result must not evict approval metadata.
    // At most 128 values of 8,000 serialized bytes; IDs have the existing
    // metadata byte ceiling. These are payload bounds, not heap estimates.
    tool_outputs: HashMap<String, RawToolOutput>,
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
            tool_metadata: HashMap::new(),
            tool_outputs: HashMap::new(),
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
        self.tool_metadata.clear();
        self.tool_outputs.clear();
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
                self.tool_outputs.clear();
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
            self.tool_outputs.clear();
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
    fn remember_tool(&mut self, update: &Value, initial: bool) -> Value {
        let Some(id) = update["toolCallId"].as_str().filter(|id| !id.is_empty()) else {
            let mut current = update.clone();
            if let Some(object) = current.as_object_mut() {
                object.remove("rawOutputTruncated");
            }
            if let Some(raw) = update.get("rawOutput").filter(|value| !value.is_null()) {
                let output = RawToolOutput::capture(raw);
                current["rawOutput"] = output.value;
                current["rawOutputTruncated"] = json!(output.truncated);
            }
            return current;
        };
        let mut current = if initial {
            json!({"toolCallId":id})
        } else {
            self.tool_metadata
                .get(id)
                .cloned()
                .unwrap_or_else(|| json!({"toolCallId":id}))
        };
        // ToolCallUpdate omission/null leaves previously reported metadata
        // unchanged (ACP v1). An explicit empty object/array does replace it.
        for key in [
            "kind",
            "title",
            "rawInput",
            "locations",
            "content",
            "status",
        ] {
            if let Some(value) = update.get(key).filter(|v| !v.is_null()) {
                current[key] = value.clone();
            }
        }
        if serde_json::to_vec(&current).is_ok_and(|bytes| bytes.len() <= TOOL_METADATA_BYTES)
            && (self.tool_metadata.contains_key(id)
                || self.tool_metadata.len() < TOOL_METADATA_COUNT)
        {
            self.tool_metadata.insert(id.to_owned(), current.clone());
        } else {
            // Oversize/new excess entries remain usable in the current frame
            // but cannot later recover an obsolete cached approval input.
            self.tool_metadata.remove(id);
        }
        if initial {
            self.tool_outputs.remove(id);
        }
        // ACP rawOutput has the same omission/null semantics as rawInput.
        // Keep its typed JSON when small enough, independently of content.
        let output = update
            .get("rawOutput")
            .filter(|value| !value.is_null())
            .map(RawToolOutput::capture)
            .or_else(|| self.tool_outputs.get(id).cloned());
        if let Some(output) = output {
            if id.len() <= TOOL_METADATA_BYTES
                && (self.tool_outputs.contains_key(id)
                    || self.tool_outputs.len() < TOOL_METADATA_COUNT)
            {
                self.tool_outputs.insert(id.to_owned(), output.clone());
            }
            // Add only to this frame's merged view, after approval metadata
            // has been cached, so output cannot change that cache's budget.
            current["rawOutput"] = output.value;
            current["rawOutputTruncated"] = json!(output.truncated);
        }
        current
    }
    fn session_update(&mut self, update: &Value) -> Step {
        match update["sessionUpdate"].as_str().unwrap_or("") {
            "agent_message_chunk" => match update["content"]["text"].as_str() {
                Some(text) if !text.is_empty() => Step::update(Update::Text(redact(text))),
                _ => Step::default(),
            },
            "tool_call" => {
                let update = self.remember_tool(update, true);
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
                    step = step.merge(self.tool_finished(&id, &name, &update));
                }
                step
            }
            "tool_call_update" => {
                let update = self.remember_tool(update, false);
                let id = update["toolCallId"].as_str().unwrap_or("").to_owned();
                let name = update["kind"]
                    .as_str()
                    .map(|kind| format!("{}.{kind}", self.vendor.id()))
                    .or_else(|| self.tool_names.get(&id).cloned())
                    .unwrap_or_else(|| format!("{}.tool", self.vendor.id()));
                match update["status"].as_str() {
                    Some("completed") | Some("failed") => self.tool_finished(&id, &name, &update),
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
        self.tool_metadata.remove(id);
        self.tool_outputs.remove(id);
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
        let mut output = redact_value(json!({
            "status": update["status"],
            "input": update["rawInput"],
            "tool_kind": update["kind"],
            "title": update["title"],
            "locations": paths,
            "content": content,
        }));
        if let Some(raw) = update.get("rawOutput").filter(|value| !value.is_null()) {
            // All output reaching the merged view is already redacted and
            // bounded. The missing-ID path skips caching, so bound it here.
            let captured = if update.get("rawOutputTruncated").is_some() {
                RawToolOutput {
                    value: raw.clone(),
                    truncated: update["rawOutputTruncated"] == true,
                }
            } else {
                RawToolOutput::capture(raw)
            };
            output["raw_output"] = captured.value;
            output["raw_output_truncated"] = json!(captured.truncated);
            output["raw_output_format"] = json!(if captured.truncated {
                "json_text_preview"
            } else {
                "json"
            });
        }
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
        let tool = self.remember_tool(&params["toolCall"], false);
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
        if matches!(
            method,
            Some("session/update" | "session/request_permission")
        ) && self
            .session_id
            .as_deref()
            .zip(message["params"]["sessionId"].as_str())
            .is_some_and(|(expected, received)| expected != received)
        {
            return Ok(Step {
                send: id
                    .map(|id| vec![error(id, -32602, "Unexpected ACP session")])
                    .unwrap_or_default(),
                updates: vec![Update::Warning(
                    "Ignored a tool/permission update for a different ACP session".into(),
                )],
            });
        }
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

#[cfg(test)]
mod metadata_tests {
    use super::*;

    fn finished_output(adapter: &mut AcpAdapter, update: Value) -> Value {
        adapter
            .session_update(&update)
            .updates
            .into_iter()
            .find_map(|update| match update {
                Update::ToolCompleted { output, .. } => Some(output),
                _ => None,
            })
            .expect("completed tool output")
    }

    #[test]
    fn acp_raw_output_merges_partial_updates_and_preserves_content_independently() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        adapter.session_update(&json!({"sessionUpdate":"tool_call","toolCallId":"output-1",
            "kind":"execute","rawOutput":{"stdout":"first"},
            "content":[{"type":"content","content":{"type":"text","text":"display content"}}]}));
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"output-1",
            "rawOutput":{"stdout":"replacement","exitCode":0}}),
        );
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"output-1","rawOutput":null}),
        );
        let output = finished_output(
            &mut adapter,
            json!({"sessionUpdate":"tool_call_update","toolCallId":"output-1","status":"completed"}),
        );
        assert_eq!(
            output["raw_output"],
            json!({"stdout":"replacement","exitCode":0})
        );
        assert_eq!(output["raw_output_truncated"], false);
        assert_eq!(output["raw_output_format"], "json");
        assert_eq!(output["content"], json!(["display content"]));
        assert!(adapter.tool_outputs.is_empty());

        for replacement in [json!({}), json!([]), json!(""), json!(false), json!(0)] {
            adapter.session_update(
                &json!({"sessionUpdate":"tool_call","toolCallId":"output-1","rawOutput":"old"}),
            );
            // Permission toolCall is also a partial update of the same tool.
            permission(
                &mut adapter,
                json!({"toolCallId":"output-1","rawOutput":replacement}),
            );
            let output = finished_output(
                &mut adapter,
                json!({"sessionUpdate":"tool_call_update","toolCallId":"output-1","status":"completed","rawOutput":null}),
            );
            assert_eq!(output["raw_output"], replacement);
        }
        let output = finished_output(
            &mut adapter,
            json!({"sessionUpdate":"tool_call","toolCallId":"no-output","status":"completed","rawOutput":null}),
        );
        assert!(output.get("raw_output").is_none());
    }

    #[test]
    fn acp_raw_output_redacts_before_bounding_without_evicting_approval_input() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        let secret = "sk-proj-fixtureABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890";
        let initial = json!({"sessionUpdate":"tool_call","toolCallId":"output-1","kind":"execute",
            "rawInput":{"command":"keep-this-input"},"rawOutput":{"access_token":"local-secret","stdout":format!("key={secret}")}});
        adapter.session_update(&initial);
        let output = finished_output(
            &mut adapter,
            json!({"sessionUpdate":"tool_call_update","toolCallId":"output-1","status":"failed"}),
        );
        assert_eq!(
            output["raw_output"]["access_token"],
            crate::redaction::placeholder()
        );
        assert!(!output.to_string().contains(secret));
        assert!(!output.to_string().contains("local-secret"));

        adapter.session_update(&initial);
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"output-1",
            "rawOutput":format!("key={secret}\n{}", "\n\\\"é".repeat(RAW_OUTPUT_BYTES))}),
        );
        assert_eq!(
            permission(&mut adapter, json!({"toolCallId":"output-1"})).arguments["input"]
                ["command"],
            "keep-this-input"
        );
        let output = finished_output(
            &mut adapter,
            json!({"sessionUpdate":"tool_call_update","toolCallId":"output-1","status":"completed"}),
        );
        assert_eq!(output["raw_output_truncated"], true);
        assert_eq!(output["raw_output_format"], "json_text_preview");
        assert!(output["raw_output"].is_string());
        assert!(output["raw_output"].to_string().len() <= RAW_OUTPUT_BYTES);
        assert!(!output.to_string().contains(secret));
        assert!(output["raw_output"]
            .as_str()
            .unwrap()
            .contains(crate::redaction::placeholder()));

        // A peer cannot spoof our internal truncation flag to bypass bounds,
        // including malformed tool notifications without an ID.
        let output = finished_output(
            &mut adapter,
            json!({"sessionUpdate":"tool_call","status":"completed",
            "rawOutput":secret.repeat(RAW_OUTPUT_BYTES),"rawOutputTruncated":false}),
        );
        assert!(output["raw_output"].to_string().len() <= RAW_OUTPUT_BYTES);
        assert!(!output.to_string().contains(secret));
    }

    #[test]
    fn acp_raw_output_cache_is_bounded_and_does_not_leak_across_tool_lifecycles() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        let initial =
            json!({"sessionUpdate":"tool_call","toolCallId":"output-1","rawOutput":"old"});
        adapter.session_update(&initial);
        adapter.session_update(&json!({"sessionUpdate":"tool_call","toolCallId":"output-1"}));
        let output = finished_output(
            &mut adapter,
            json!({"sessionUpdate":"tool_call_update","toolCallId":"output-1","status":"completed"}),
        );
        assert!(output.get("raw_output").is_none());
        adapter.session_update(&initial);
        adapter.session_id = Some("current".into());
        adapter.start_prompt("fresh prompt", &[]).unwrap();
        assert!(adapter.tool_outputs.is_empty());
        adapter.session_update(&initial);
        adapter.on_line(&json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"foreign","update":{
            "sessionUpdate":"tool_call_update","toolCallId":"output-1","rawOutput":"foreign"}}}).to_string()).unwrap();
        let output = finished_output(
            &mut adapter,
            json!({"sessionUpdate":"tool_call_update","toolCallId":"output-1","status":"completed"}),
        );
        assert_eq!(output["raw_output"], "old");

        for i in 0..TOOL_METADATA_COUNT + 5 {
            adapter.session_update(&json!({"sessionUpdate":"tool_call","toolCallId":format!("output-{i}"),"rawOutput":"x".repeat(RAW_OUTPUT_BYTES * 2)}));
        }
        assert_eq!(adapter.tool_outputs.len(), TOOL_METADATA_COUNT);
        assert!(adapter
            .tool_outputs
            .values()
            .all(|output| output.value.to_string().len() <= RAW_OUTPUT_BYTES));
        // A terminal frame's own output still works when retention is full.
        let output = finished_output(
            &mut adapter,
            json!({"sessionUpdate":"tool_call_update","toolCallId":"excess","status":"completed","rawOutput":{"stdout":"current"}}),
        );
        assert_eq!(output["raw_output"]["stdout"], "current");
        let output = finished_output(
            &mut adapter,
            json!({"sessionUpdate":"tool_call_update","toolCallId":"output-132","status":"completed"}),
        );
        assert!(output.get("raw_output").is_none());
        let prompt_id = adapter.prompt_id.unwrap();
        adapter
            .handle_response(prompt_id, &json!({"result":{"stopReason":"end_turn"}}))
            .unwrap();
        assert!(adapter.tool_outputs.is_empty());
    }

    fn permission(adapter: &mut AcpAdapter, tool: Value) -> ApprovalPrompt {
        adapter.permission_request(&json!(1), &json!({
            "toolCall":tool,
            "options":[{"optionId":"yes","kind":"allow_once"},{"optionId":"no","kind":"reject_once"}]
        })).updates.into_iter().find_map(|u| match u {
            Update::Approval(prompt) => Some(prompt), _ => None,
        }).unwrap()
    }

    #[test]
    fn acp_partial_permission_uses_latest_typed_input_not_initial_command_or_title() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        adapter.session_update(&json!({"sessionUpdate":"tool_call","toolCallId":"call-1",
            "kind":"execute","title":"Run original","rawInput":{"command":"original","cwd":"/old"}}));
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"call-1",
            "rawInput":{"command":"replacement","cwd":"/new"}}),
        );
        let prompt = permission(
            &mut adapter,
            json!({"toolCallId":"call-1","title":"Just a display label","rawInput":null}),
        );
        assert_eq!(prompt.tool, "cursor.execute");
        assert_eq!(prompt.command, "replacement");
        assert_eq!(
            prompt.arguments["input"],
            json!({"command":"replacement","cwd":"/new"})
        );
        assert_eq!(prompt.arguments["title"], "Just a display label");
        let override_prompt = permission(
            &mut adapter,
            json!({"toolCallId":"call-1","rawInput":{"command":"third"}}),
        );
        assert_eq!(
            override_prompt.arguments["input"],
            json!({"command":"third"})
        );
        let empty = permission(&mut adapter, json!({"toolCallId":"call-1","rawInput":{}}));
        assert_eq!(empty.arguments["input"], json!({}));
        assert_ne!(empty.command, "third");
    }

    #[test]
    fn acp_metadata_does_not_survive_completion_new_prompt_or_duplicate_initial_call() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        let initial = json!({"sessionUpdate":"tool_call","toolCallId":"call-1","kind":"execute","rawInput":{"command":"old"}});
        adapter.session_update(&initial);
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"call-1","status":"completed"}),
        );
        assert!(
            permission(&mut adapter, json!({"toolCallId":"call-1"})).arguments["input"].is_null()
        );
        adapter.session_update(&initial);
        adapter.session_id = Some("session".into());
        adapter.start_prompt("new task", &[]).unwrap();
        assert!(
            permission(&mut adapter, json!({"toolCallId":"call-1"})).arguments["input"].is_null()
        );
        adapter.session_update(&initial);
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call","toolCallId":"call-1","kind":"execute"}),
        );
        assert!(
            permission(&mut adapter, json!({"toolCallId":"call-1"})).arguments["input"].is_null()
        );
        assert!(
            permission(&mut adapter, json!({"toolCallId":"unknown"})).arguments["input"].is_null()
        );
    }

    #[test]
    fn acp_metadata_is_bounded_and_drops_old_input_after_oversize_replacement() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        adapter.remember_tool(
            &json!({"toolCallId":"kept","rawInput":{"command":"old"}}),
            true,
        );
        adapter.remember_tool(
            &json!({"toolCallId":"kept","rawInput":{"command":"x".repeat(TOOL_METADATA_BYTES)}}),
            false,
        );
        assert!(
            permission(&mut adapter, json!({"toolCallId":"kept"})).arguments["input"].is_null()
        );
        for i in 0..TOOL_METADATA_COUNT + 5 {
            adapter.remember_tool(
                &json!({"toolCallId":format!("call-{i}"),"rawInput":{"command":"test"}}),
                true,
            );
        }
        assert_eq!(adapter.tool_metadata.len(), TOOL_METADATA_COUNT);
        assert!(
            permission(&mut adapter, json!({"toolCallId":"call-132"})).arguments["input"].is_null()
        );
        assert!(adapter
            .tool_metadata
            .values()
            .all(|v| serde_json::to_vec(v).unwrap().len() <= TOOL_METADATA_BYTES));
    }

    #[test]
    fn acp_cached_permission_input_is_still_redacted_before_display() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        let secret = "sk-proj-fixtureABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890";
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call","toolCallId":"secret-check",
            "kind":"execute","rawInput":{"command":format!("echo {secret}")}}),
        );
        let prompt = permission(&mut adapter, json!({"toolCallId":"secret-check"}));
        assert!(!prompt.command.contains(secret));
        assert!(!prompt.arguments.to_string().contains(secret));
        assert!(prompt.arguments["input"]["command"]
            .as_str()
            .unwrap()
            .contains("[redacted secret]"));
    }

    #[test]
    fn acp_completion_retains_latest_input_and_permission_kind() {
        let mut adapter = AcpAdapter::new(Vendor::Grok);
        adapter.session_update(&json!({"sessionUpdate":"tool_call","toolCallId":"call-1","rawInput":{"command":"old"}}));
        permission(
            &mut adapter,
            json!({"toolCallId":"call-1","kind":"execute","rawInput":{"command":"actual","cwd":"/project"}}),
        );
        let completed = adapter.session_update(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"call-1","status":"completed"}),
        );
        assert!(
            matches!(&completed.updates[0], Update::ToolCompleted { id, name, output, .. }
            if id == "call-1" && name == "grok.execute" && output["input"] == json!({"command":"actual","cwd":"/project"}) && output["tool_kind"] == "execute")
        );
    }

    #[test]
    fn acp_foreign_session_cannot_change_or_borrow_current_tool_metadata() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        adapter.session_id = Some("current".into());
        adapter.session_update(&json!({"sessionUpdate":"tool_call","toolCallId":"same-id","kind":"execute","rawInput":{"command":"current-command"}}));
        let foreign = adapter.on_line(&json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"foreign","update":{"sessionUpdate":"tool_call_update","toolCallId":"same-id","rawInput":{"command":"foreign-command"}}}}).to_string()).unwrap();
        assert!(foreign
            .updates
            .iter()
            .all(|u| matches!(u, Update::Warning(_))));
        let denied = adapter.on_line(&json!({"jsonrpc":"2.0","id":42,"method":"session/request_permission","params":{"sessionId":"foreign","toolCall":{"toolCallId":"same-id"},"options":[{"optionId":"yes","kind":"allow_once"}]}}).to_string()).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&denied.send[0]).unwrap()["error"]["code"],
            -32602
        );
        assert!(denied
            .updates
            .iter()
            .all(|u| matches!(u, Update::Warning(_))));
        assert!(adapter.approve("42", true).is_err());
        assert_eq!(
            permission(&mut adapter, json!({"toolCallId":"same-id"})).arguments["input"]["command"],
            "current-command"
        );
    }
}

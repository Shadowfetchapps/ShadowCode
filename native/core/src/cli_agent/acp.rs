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
use std::collections::{HashMap, HashSet};

const OUTPUT_PREVIEW: usize = 8000;
const TOOL_METADATA_COUNT: usize = 128;
const TOOL_METADATA_BYTES: usize = 64_000;
const RAW_OUTPUT_BYTES: usize = 8000;
const OBSERVED_TOOL_IDS: usize = 4096;

#[derive(Clone)]
struct RawToolOutput {
    value: Value,
    truncated: bool,
    cursor_exit_code: Option<i64>,
}

impl RawToolOutput {
    fn capture(value: &Value) -> Self {
        // Redact before clipping, including known credential keys: clipping
        // first could leave a token prefix that no longer matches redaction.
        // Installed Cursor ACP exposes shell success AND failure as this
        // exact shape, with status "completed" in both cases. Capture the
        // integer before clipping so a large failing output cannot turn green.
        let cursor_exit_code = value.as_object().filter(|o| o.len() == 3).and_then(|o| {
            o.get("stdout")?.as_str()?;
            o.get("stderr")?.as_str()?;
            o.get("exitCode")?.as_i64()
        });
        let mut value = value.clone();
        crate::redaction::redact_known_secrets(&mut value);
        let value = redact_value(value);
        let encoded = value.to_string();
        if encoded.len() <= RAW_OUTPUT_BYTES {
            return Self {
                value,
                truncated: false,
                cursor_exit_code,
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
            cursor_exit_code,
        }
    }
}

// An approval displays proposed changes as well as typed input. Bind those
// bytes/targets for non-execute operations, including generic tool kinds.
// Ordinary shell text is diagnostic/output (Cursor retains its pre-approval
// allowlist reason); it must not turn a later shell result into a new command.
// A shell's explicit diff proposal is still approval-relevant if present.
fn operation_identity(tool: &Value) -> Value {
    let proposal = if tool["kind"] == "execute" {
        let diffs: Vec<_> = tool["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|block| block["type"] == "diff")
            .collect();
        if diffs.is_empty() {
            Value::Null
        } else {
            json!({"content":diffs,"locations":tool["locations"]})
        }
    } else {
        json!({"content":tool["content"],"locations":tool["locations"]})
    };
    json!([tool["kind"], tool["rawInput"], proposal])
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

#[derive(Clone, Copy, PartialEq)]
enum ToolOrigin {
    Initial,
    Update,
    Permission,
}
impl ToolOrigin {
    fn label(self) -> &'static str {
        match self {
            Self::Initial => "tool_call",
            Self::Update => "tool_update",
            Self::Permission => "permission_request",
        }
    }
}
#[derive(Clone)]
struct FieldOrigin {
    source: ToolOrigin,
    phase: Option<String>,
    revision: u64,
    explicit_terminal: bool,
}
#[derive(Clone)]
struct PermissionBinding {
    tool_id: String,
    generation: u64,
    operation_revision: u64,
    request_revision: u64,
}
#[derive(Clone)]
struct PermissionDecision {
    binding: PermissionBinding,
    kind: &'static str,
    revision: u64,
}
#[derive(Clone)]
struct ToolEvidence {
    // Only an explicit initial call starts complete history. Eviction must
    // never make an earlier denied permission look like no permission asked.
    history_complete: bool,
    generation: u64,
    revision: u64,
    operation_hash: Option<String>,
    operation_revision: u64,
    permission_revision: Option<u64>,
    permission_usable: bool,
    content: Option<FieldOrigin>,
    raw_output: Option<FieldOrigin>,
    decision: Option<PermissionDecision>,
}
impl ToolEvidence {
    fn matches(&self, binding: &PermissionBinding) -> bool {
        self.operation_hash.is_some()
            && self.generation == binding.generation
            && self.operation_revision == binding.operation_revision
            && self.permission_revision == Some(binding.request_revision)
    }
    fn json(&self) -> Value {
        let decision = self.decision.as_ref();
        let field = |origin: &Option<FieldOrigin>| {
            origin.as_ref().map(|origin| json!({
            "source": origin.source.label(), "phase": origin.phase,
            "explicit_terminal": origin.explicit_terminal && origin.revision == self.revision,
            "before_permission_decision": decision.is_some_and(|d| origin.revision < d.revision),
            "after_permission_decision": decision.is_some_and(|d| origin.revision > d.revision),
        }))
        };
        json!({
            "schema_version": 1,
            "history_complete": self.history_complete,
            "content": field(&self.content),
            "raw_output": field(&self.raw_output),
            "permission": decision.map(|d| json!({
                "state": "resolved", "decision": d.kind,
                "current_operation_matches": self.matches(&d.binding),
            })).or_else(|| self.permission_revision.map(|_| json!({
                "state": if self.permission_usable { "pending" } else { "declined" },
                "decision": if self.permission_usable { Value::Null } else { json!("unusable_options") },
                "current_operation_matches": false,
            }))),
        })
    }
}
// This typed sidecar is created only by the adapter; protocol JSON cannot
// supply provenance, execution status, or permission-correlation flags.
struct RememberedTool {
    metadata: Value,
    raw_output: Option<RawToolOutput>,
    evidence: Option<ToolEvidence>,
}
struct PermissionChoice {
    id: String,
    kind: &'static str,
}
struct PendingPermission {
    allow: Option<PermissionChoice>,
    reject: Option<PermissionChoice>,
    binding: Option<PermissionBinding>,
    require_binding: bool,
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
    // Small provenance sidecars share the metadata count and ID bounds.
    // Decisions retain the bounded call ID; revisions distinguish a changed-
    // then-restored operation. No input or result payload is duplicated.
    tool_evidence: HashMap<String, ToolEvidence>,
    evidence_revision: u64,
    // Hashed IDs are bounded independently of live metadata. A permission-
    // first or evicted ID must never become a permission-free initial call.
    // Once full, new histories remain incomplete for the rest of the prompt.
    observed_tool_ids: HashSet<String>,
    tool_history_exhausted: bool,
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
            tool_evidence: HashMap::new(),
            evidence_revision: 0,
            observed_tool_ids: HashSet::new(),
            tool_history_exhausted: false,
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
        self.tool_outputs.clear();
        self.tool_evidence.clear();
        self.observed_tool_ids.clear();
        self.tool_history_exhausted = false;
        self.tool_metadata.clear();
        self.tool_names.clear();
        self.pending_permissions.clear();
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
                self.tool_evidence.clear();
                self.tool_metadata.clear();
                self.tool_names.clear();
                self.pending_permissions.clear();
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
            if let Err(error) = super::acp_probe::require_protocol_v1(res) {
                bail!(
                    "{} runtime returned an {error}",
                    self.vendor.product_label()
                );
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
            self.tool_evidence.clear();
            self.tool_metadata.clear();
            self.tool_names.clear();
            self.pending_permissions.clear();
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
    fn evidence_id(&mut self) -> u64 {
        self.evidence_revision = self.evidence_revision.saturating_add(1);
        self.evidence_revision
    }
    fn remember_tool(&mut self, update: &Value, source: ToolOrigin) -> RememberedTool {
        let initial = source == ToolOrigin::Initial;
        let id = update["toolCallId"].as_str().filter(|id| !id.is_empty());
        let first_observation = id.is_some_and(|id| {
            let digest = crate::workspace::hash(id.as_bytes());
            if self.observed_tool_ids.contains(&digest) {
                false
            } else if self.observed_tool_ids.len() < OBSERVED_TOOL_IDS {
                self.observed_tool_ids.insert(digest);
                !self.tool_history_exhausted
            } else {
                self.tool_history_exhausted = true;
                false
            }
        });
        let mut current = if initial {
            json!({"toolCallId":id})
        } else {
            id.and_then(|id| self.tool_metadata.get(id))
                .cloned()
                .unwrap_or_else(|| json!({"toolCallId":id}))
        };
        // ToolCallUpdate omission/null leaves previous metadata unchanged
        // (ACP v1). Explicit empty values replace it. Only protocol fields
        // enter this view: a peer cannot forge our sidecar or output flags.
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
        let retained = id.is_some_and(|id| {
            serde_json::to_vec(&current).is_ok_and(|bytes| bytes.len() <= TOOL_METADATA_BYTES)
                && (self.tool_metadata.contains_key(id)
                    || self.tool_metadata.len() < TOOL_METADATA_COUNT)
        });
        let revision = self.evidence_id();
        let mut evidence = if retained {
            let hash = (!current["rawInput"].is_null() && current["kind"].is_string()).then(|| {
                crate::workspace::hash(operation_identity(&current).to_string().as_bytes())
            });
            let mut evidence = (!initial)
                .then(|| id.and_then(|id| self.tool_evidence.get(id)).cloned())
                .flatten()
                .unwrap_or(ToolEvidence {
                    history_complete: initial && first_observation,
                    generation: revision,
                    revision,
                    operation_hash: None,
                    operation_revision: revision,
                    permission_revision: None,
                    permission_usable: true,
                    content: None,
                    raw_output: None,
                    decision: None,
                });
            if evidence.operation_hash != hash {
                evidence.operation_revision = revision;
                evidence.operation_hash = hash;
            }
            evidence.revision = revision;
            let origin = FieldOrigin {
                source,
                revision,
                explicit_terminal: source != ToolOrigin::Permission
                    && matches!(update["status"].as_str(), Some("completed" | "failed")),
                // Bound this host-retained string; arbitrary status text is
                // never meaningful provenance for a command result.
                phase: current["status"]
                    .as_str()
                    .filter(|s| matches!(*s, "pending" | "in_progress" | "completed" | "failed"))
                    .map(str::to_owned),
            };
            if update.get("content").is_some_and(|v| !v.is_null()) {
                evidence.content = Some(origin.clone());
            }
            if update.get("rawOutput").is_some_and(|v| !v.is_null()) {
                evidence.raw_output = Some(origin);
            }
            if source == ToolOrigin::Permission {
                evidence.permission_revision = Some(revision);
                evidence.permission_usable = true;
                evidence.decision = None;
            }
            Some(evidence)
        } else {
            None
        };
        if let Some(id) = id {
            if retained {
                self.tool_metadata.insert(id.to_owned(), current.clone());
                self.tool_evidence
                    .insert(id.to_owned(), evidence.as_ref().unwrap().clone());
            } else {
                // Never recover obsolete input/provenance after eviction.
                self.tool_metadata.remove(id);
                self.tool_evidence.remove(id);
                evidence = None;
            }
            if initial {
                self.tool_outputs.remove(id);
            }
        }
        let output = update
            .get("rawOutput")
            .filter(|v| !v.is_null())
            .map(RawToolOutput::capture)
            .or_else(|| id.and_then(|id| self.tool_outputs.get(id)).cloned());
        if let (Some(id), Some(output)) = (id, &output) {
            if id.len() <= TOOL_METADATA_BYTES
                && (self.tool_outputs.contains_key(id)
                    || self.tool_outputs.len() < TOOL_METADATA_COUNT)
            {
                self.tool_outputs.insert(id.to_owned(), output.clone());
            }
        }
        RememberedTool {
            metadata: current,
            raw_output: output,
            evidence,
        }
    }
    fn session_update(&mut self, update: &Value) -> Step {
        match update["sessionUpdate"].as_str().unwrap_or("") {
            "agent_message_chunk" => match update["content"]["text"].as_str() {
                Some(text) if !text.is_empty() => Step::update(Update::Text(redact(text))),
                _ => Step::default(),
            },
            "tool_call" => {
                let remembered = self.remember_tool(update, ToolOrigin::Initial);
                let update = &remembered.metadata;
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
                    step = step.merge(self.tool_finished(&id, &name, &remembered));
                }
                step
            }
            "tool_call_update" => {
                let remembered = self.remember_tool(update, ToolOrigin::Update);
                let update = &remembered.metadata;
                let id = update["toolCallId"].as_str().unwrap_or("").to_owned();
                let name = update["kind"]
                    .as_str()
                    .map(|kind| format!("{}.{kind}", self.vendor.id()))
                    .or_else(|| self.tool_names.get(&id).cloned())
                    .unwrap_or_else(|| format!("{}.tool", self.vendor.id()));
                match update["status"].as_str() {
                    Some("completed") | Some("failed") => {
                        self.tool_finished(&id, &name, &remembered)
                    }
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
    fn tool_finished(&mut self, id: &str, name: &str, remembered: &RememberedTool) -> Step {
        let update = &remembered.metadata;
        self.tool_metadata.remove(id);
        self.tool_outputs.remove(id);
        self.tool_evidence.remove(id);
        self.tool_names.remove(id);
        let cursor_exit = (self.vendor == Vendor::Cursor && name == "cursor.execute")
            .then(|| {
                remembered
                    .raw_output
                    .as_ref()
                    .and_then(|o| o.cursor_exit_code)
            })
            .flatten();
        let success = update["status"].as_str() == Some("completed")
            && cursor_exit.is_none_or(|code| code == 0);
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
        if let Some(evidence) = &remembered.evidence {
            output["acp_provenance"] = evidence.json();
        }
        if let Some(code) = cursor_exit {
            output["cursor_execution"] = json!({"exit_code":code});
        }
        if let Some(captured) = &remembered.raw_output {
            output["raw_output"] = captured.value.clone();
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
        let key = id.to_string();
        if self.pending_permissions.remove(&key).is_some() {
            // The outstanding user decision belongs to the original request.
            // A malformed/question duplicate may not emit Approval at all, so
            // reject it here before any early return or replacement binding.
            return Step {
                send: vec![error(
                    id,
                    -32600,
                    "Duplicate outstanding permission request ID",
                )],
                updates: vec![Update::TurnFailed(format!(
                    "{} reused an outstanding permission request ID; no approval was granted",
                    self.vendor.id()
                ))],
            };
        }
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
        // Record even an unusable request: declining malformed options must
        // not later look as if the call never required permission.
        let remembered = self.remember_tool(&params["toolCall"], ToolOrigin::Permission);
        let mut allow = None;
        let mut reject = None;
        for option in params["options"].as_array().into_iter().flatten() {
            let Some(option_id) = option["optionId"].as_str() else {
                continue;
            };
            match option["kind"].as_str() {
                Some("allow_once") => {
                    allow = Some(PermissionChoice {
                        id: option_id.into(),
                        kind: "allow_once",
                    })
                }
                Some("allow_always") if allow.is_none() => {
                    allow = Some(PermissionChoice {
                        id: option_id.into(),
                        kind: "allow_always",
                    })
                }
                Some("reject_once") => {
                    reject = Some(PermissionChoice {
                        id: option_id.into(),
                        kind: "reject_once",
                    })
                }
                Some("reject_always") if reject.is_none() => {
                    reject = Some(PermissionChoice {
                        id: option_id.into(),
                        kind: "reject_always",
                    })
                }
                _ => {}
            }
        }
        if allow.is_none() && reject.is_none() {
            if let Some(tool_id) = params["toolCall"]["toolCallId"].as_str() {
                if let Some(evidence) = self.tool_evidence.get_mut(tool_id) {
                    evidence.permission_usable = false;
                }
            }
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
        let tool = &remembered.metadata;
        let title = tool["title"].as_str().unwrap_or("tool call");
        let kind = tool["kind"].as_str().unwrap_or("tool");
        let binding = remembered
            .evidence
            .as_ref()
            .filter(|e| e.operation_hash.is_some())
            .map(|e| PermissionBinding {
                tool_id: tool["toolCallId"].as_str().unwrap_or_default().to_owned(),
                generation: e.generation,
                operation_revision: e.operation_revision,
                request_revision: e.revision,
            });
        self.pending_permissions.insert(
            key.clone(),
            PendingPermission {
                allow,
                reject,
                binding,
                // Cursor approvals must refer to a retained typed operation.
                // Requiring this for every kind prevents a generic/read call
                // from changing into execute while its approval is open.
                require_binding: self.vendor == Vendor::Cursor,
            },
        );
        let command = tool["rawInput"]["command"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| title.to_owned());
        Step::update(Update::Approval(ApprovalPrompt {
            request_id: key,
            tool_identity: tool["toolCallId"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(super::approval_tool_identity),
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
        let current = pending
            .binding
            .as_ref()
            .and_then(|b| self.tool_evidence.get(&b.tool_id));
        let matches = pending
            .binding
            .as_ref()
            .zip(current)
            .is_some_and(|(b, e)| e.matches(b));
        let choice = if approve && pending.require_binding && !matches {
            // Approval covered the displayed operation, never a new command,
            // repeated call ID, evicted input, or a newer permission request.
            None
        } else if approve {
            pending.allow
        } else {
            pending.reject
        };
        let revision = self.evidence_id();
        if let Some(binding) = pending.binding {
            if let Some(evidence) = self.tool_evidence.get_mut(&binding.tool_id) {
                if evidence.generation == binding.generation
                    && evidence.permission_revision == Some(binding.request_revision)
                {
                    evidence.decision = Some(PermissionDecision {
                        binding,
                        revision,
                        kind: choice.as_ref().map(|c| c.kind).unwrap_or("cancelled"),
                    });
                }
            }
        }
        Ok(vec![match choice {
            Some(option) => result(
                &id,
                json!({"outcome":{"outcome":"selected","optionId":option.id}}),
            ),
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

    fn cursor_start(adapter: &mut AcpAdapter) {
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call", "toolCallId":"shell-one",
            "kind":"execute", "status":"pending", "rawInput":{"command":"python3 -m unittest -q"}}),
        );
    }
    fn cursor_permission(adapter: &mut AcpAdapter) {
        permission(
            adapter,
            json!({"toolCallId":"shell-one", "status":"pending",
            "content":[{"type":"content","content":{"type":"text","text":"Not in allowlist: python3"}}]}),
        );
    }
    fn cursor_terminal(adapter: &mut AcpAdapter, code: i64, large: bool) -> (bool, Value) {
        adapter.session_update(&json!({"sessionUpdate":"tool_call_update", "toolCallId":"shell-one",
            "status":"completed", "rawOutput":{"exitCode":code,"stdout":if large {"x".repeat(RAW_OUTPUT_BYTES * 2)} else {String::new()},
            "stderr":"----------------------------------------------------------------------\nRan 5 tests in 0.000s\n\nOK\n"}}))
            .updates.into_iter().find_map(|u| match u {
                Update::ToolCompleted { success, output, .. } => Some((success, output)), _ => None,
            }).unwrap()
    }

    #[test]
    fn cursor_shell_provenance_separates_earlier_permission_reason_from_terminal_result() {
        // Sequence reproduced by the installed Cursor 2026.09.15 presenter:
        // permission reason in content; approved shell result in rawOutput;
        // terminal update omits content, so ACP correctly retains the reason.
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        cursor_start(&mut adapter);
        cursor_permission(&mut adapter);
        assert!(adapter.approve("1", true).unwrap()[0].contains("selected"));
        let (success, output) = cursor_terminal(&mut adapter, 0, false);
        assert!(success);
        assert_eq!(output["content"], json!(["Not in allowlist: python3"]));
        assert_eq!(output["cursor_execution"]["exit_code"], 0);
        assert_eq!(
            output["acp_provenance"],
            json!({
                "schema_version":1,
                "history_complete":true,
                "content":{"source":"permission_request", "phase":"pending", "explicit_terminal":false,
                    "before_permission_decision":true,"after_permission_decision":false},
                "raw_output":{"source":"tool_update", "phase":"completed", "explicit_terminal":true,
                    "before_permission_decision":false,"after_permission_decision":true},
                "permission":{"state":"resolved","decision":"allow_once","current_operation_matches":true}
            })
        );
        assert!(adapter.tool_evidence.is_empty());
    }

    #[test]
    fn cursor_nonzero_shell_exit_fails_without_permission_even_when_output_is_truncated() {
        for large in [false, true] {
            let mut adapter = AcpAdapter::new(Vendor::Cursor);
            cursor_start(&mut adapter);
            let (success, output) = cursor_terminal(&mut adapter, 7, large);
            assert!(!success, "completed ACP status must not hide exit 7");
            assert_eq!(output["cursor_execution"]["exit_code"], 7);
            assert_eq!(output["raw_output_truncated"], large);
            assert!(output["acp_provenance"]["permission"].is_null());
        }
        let mut other = AcpAdapter::new(Vendor::Grok);
        cursor_start(&mut other);
        let (success, output) = cursor_terminal(&mut other, 7, false);
        assert!(success, "Cursor shell shape is not a cross-vendor contract");
        assert!(output.get("cursor_execution").is_none());
    }

    #[test]
    fn cursor_approval_cannot_authorize_changed_then_restored_operation() {
        for decision_before_change in [false, true] {
            let mut adapter = AcpAdapter::new(Vendor::Cursor);
            cursor_start(&mut adapter);
            cursor_permission(&mut adapter);
            if decision_before_change {
                adapter.approve("1", true).unwrap();
            }
            for command in ["other", "python3 -m unittest -q"] {
                adapter.session_update(
                    &json!({"sessionUpdate":"tool_call_update", "toolCallId":"shell-one",
                    "rawInput":{"command":command}}),
                );
            }
            if !decision_before_change {
                assert!(adapter.approve("1", true).unwrap()[0].contains("cancelled"));
            }
            let (_, output) = cursor_terminal(&mut adapter, 0, false);
            assert_eq!(
                output["acp_provenance"]["permission"]["current_operation_matches"],
                false
            );
            assert_eq!(
                output["acp_provenance"]["permission"]["decision"],
                if decision_before_change {
                    "allow_once"
                } else {
                    "cancelled"
                }
            );
        }
    }

    #[test]
    fn cursor_approval_binds_non_execute_kinds_and_refuses_missing_input() {
        for kind in ["read", "tool", "execute"] {
            for input in [json!({"path":"helpers.py"}), Value::Null] {
                let mut adapter = AcpAdapter::new(Vendor::Cursor);
                adapter.session_update(
                    &json!({"sessionUpdate":"tool_call","toolCallId":"shell-one",
                    "kind":kind,"rawInput":input}),
                );
                cursor_permission(&mut adapter);
                adapter.session_update(
                    &json!({"sessionUpdate":"tool_call_update","toolCallId":"shell-one",
                    "kind":"execute","rawInput":{"command":"different command"}}),
                );
                assert!(adapter.approve("1", true).unwrap()[0].contains("cancelled"));
            }
        }
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        permission(
            &mut adapter,
            json!({"toolCallId":"unknown","kind":"tool","title":"Just a title"}),
        );
        assert!(adapter.approve("1", true).unwrap()[0].contains("cancelled"));
        permission(
            &mut adapter,
            json!({"toolCallId":"read-one","kind":"read","rawInput":{"path":"helpers.py"}}),
        );
        assert!(adapter.approve("1", true).unwrap()[0].contains("selected"));
    }

    #[test]
    fn cursor_approval_binds_displayed_edit_proposal_and_targets() {
        // Adapter-level guarantee: these updates have already been delivered
        // to on_line/session_update before the approval response is selected.
        for kind in ["edit", "delete", "move", "other"] {
            let content =
                json!([{"type":"diff","path":"helpers.py","oldText":"old","newText":"approved"}]);
            let locations = json!([{"path":"helpers.py"}]);
            for mutation in [
                Value::Null,
                json!({"content":[{"type":"diff","path":"helpers.py","oldText":"old","newText":"changed"}]}),
                json!({"locations":[{"path":"outside.py"}]}),
            ] {
                let mut adapter = AcpAdapter::new(Vendor::Cursor);
                adapter.session_update(&json!({"sessionUpdate":"tool_call","toolCallId":"edit-one",
                    "kind":kind,"rawInput":{"path":"helpers.py"},"content":content,"locations":locations}));
                permission(
                    &mut adapter,
                    json!({"toolCallId":"edit-one","status":"pending"}),
                );
                let mut update =
                    json!({"sessionUpdate":"tool_call_update","toolCallId":"edit-one"});
                if let Some(fields) = mutation.as_object() {
                    for (key, value) in fields {
                        update[key] = value.clone();
                    }
                }
                adapter.session_update(&update);
                let response = adapter.approve("1", true).unwrap();
                assert!(
                    response[0].contains(if mutation.is_null() {
                        "selected"
                    } else {
                        "cancelled"
                    }),
                    "{kind}: {mutation}"
                );
            }
        }
    }

    #[test]
    fn cursor_shell_diagnostics_are_not_identity_but_displayed_diffs_are() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        cursor_start(&mut adapter);
        cursor_permission(&mut adapter);
        adapter.session_update(&json!({"sessionUpdate":"tool_call_update","toolCallId":"shell-one",
            "content":[{"type":"content","content":{"type":"text","text":"Updated permission explanation"}}]}));
        assert!(adapter.approve("1", true).unwrap()[0].contains("selected"));
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"shell-one",
            "content":[{"type":"content","content":{"type":"text","text":"Command output"}}]}),
        );
        let (_, output) = cursor_terminal(&mut adapter, 0, false);
        assert_eq!(
            output["acp_provenance"]["permission"]["current_operation_matches"],
            true
        );

        cursor_start(&mut adapter);
        permission(
            &mut adapter,
            json!({"toolCallId":"shell-one",
            "content":[{"type":"diff","path":"helpers.py","newText":"approved"}]}),
        );
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"shell-one",
            "content":[{"type":"diff","path":"helpers.py","newText":"different"}]}),
        );
        assert!(adapter.approve("1", true).unwrap()[0].contains("cancelled"));
    }

    #[test]
    fn cursor_permission_binding_cannot_cross_reused_ids_new_prompts_or_requests() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        cursor_start(&mut adapter);
        cursor_permission(&mut adapter);
        cursor_start(&mut adapter);
        assert!(adapter.approve("1", true).unwrap()[0].contains("cancelled"));
        cursor_permission(&mut adapter);
        adapter.session_id = Some("session-one".into());
        adapter.start_prompt("new task", &[]).unwrap();
        assert!(adapter.approve("1", true).is_err());
        assert!(adapter.tool_evidence.is_empty());

        cursor_start(&mut adapter);
        cursor_permission(&mut adapter);
        adapter.permission_request(
            &json!(2),
            &json!({"toolCall":{"toolCallId":"shell-one"},
            "options":[{"optionId":"yes","kind":"allow_once"}]}),
        );
        assert!(adapter.approve("1", true).unwrap()[0].contains("cancelled"));
        assert!(adapter.approve("2", true).unwrap()[0].contains("selected"));
    }

    #[test]
    fn cursor_evicted_permission_history_cannot_reappear_as_never_requested() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        cursor_start(&mut adapter);
        cursor_permission(&mut adapter);
        assert!(adapter.approve("1", false).unwrap()[0].contains("selected"));
        adapter.session_update(&json!({"sessionUpdate":"tool_call_update","toolCallId":"shell-one",
            "content":[{"type":"content","content":{"type":"text","text":"x".repeat(TOOL_METADATA_BYTES)}}]}));
        assert!(!adapter.tool_evidence.contains_key("shell-one"));
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"shell-one",
            "kind":"execute","rawInput":{"command":"python3 -m unittest -q"},"content":[]}),
        );
        let (_, output) = cursor_terminal(&mut adapter, 0, false);
        assert_eq!(output["tool_kind"], "execute");
        assert_eq!(output["acp_provenance"]["history_complete"], false);
        assert!(output["acp_provenance"]["permission"].is_null());
        assert_eq!(output["raw_output"]["exitCode"], 0);
        // An initial call for the old ID cannot launder earlier history.
        cursor_start(&mut adapter);
        let (_, output) = cursor_terminal(&mut adapter, 0, false);
        assert_eq!(output["acp_provenance"]["history_complete"], false);
    }

    #[test]
    fn duplicate_permission_id_cannot_keep_old_authority_through_early_returns() {
        for (vendor, tool, options) in [
            (
                Vendor::Cursor,
                json!({"toolCallId":"other","kind":"execute","rawInput":{"command":"different"}}),
                json!([]),
            ),
            (
                Vendor::Cursor,
                json!({"toolCallId":"other","kind":"execute","rawInput":{"command":"different"}}),
                json!([{"optionId":"yes","kind":"allow_once"}]),
            ),
            (
                Vendor::Antigravity,
                json!({"toolCallId":"interaction_other","title":"A question"}),
                json!([{"optionId":"yes","kind":"allow_once"}]),
            ),
        ] {
            let mut adapter = AcpAdapter::new(vendor);
            permission(
                &mut adapter,
                json!({"toolCallId":"original","kind":"execute","rawInput":{"command":"original"}}),
            );
            let duplicate =
                adapter.permission_request(&json!(1), &json!({"toolCall":tool,"options":options}));
            assert!(duplicate
                .updates
                .iter()
                .any(|update| matches!(update, Update::TurnFailed(_))));
            assert!(duplicate.send[0].contains("Duplicate outstanding permission request ID"));
            assert!(adapter.approve("1", true).is_err());
        }
    }

    #[test]
    fn cursor_unanswered_or_unusable_permission_never_looks_unrequested() {
        for scenario in ["pending", "reject_then_pending", "unusable"] {
            let mut adapter = AcpAdapter::new(Vendor::Cursor);
            cursor_start(&mut adapter);
            if scenario == "unusable" {
                let step = adapter.permission_request(
                    &json!(1),
                    &json!({
                    "toolCall":{"toolCallId":"shell-one","status":"pending"},"options":[]}),
                );
                assert!(step.send[0].contains("No usable permission options"));
            } else {
                cursor_permission(&mut adapter);
                if scenario == "reject_then_pending" {
                    adapter.approve("1", false).unwrap();
                    cursor_permission(&mut adapter);
                }
            }
            adapter.session_update(
                &json!({"sessionUpdate":"tool_call_update","toolCallId":"shell-one","content":[]}),
            );
            let (_, output) = cursor_terminal(&mut adapter, 0, false);
            let permission = &output["acp_provenance"]["permission"];
            assert_eq!(
                permission["state"],
                if scenario == "unusable" {
                    "declined"
                } else {
                    "pending"
                }
            );
            assert_eq!(permission["current_operation_matches"], false);
            assert_eq!(output["acp_provenance"]["history_complete"], true);
        }
    }

    #[test]
    fn cursor_permission_first_and_evicted_ids_cannot_start_complete_history() {
        for evict in [false, true] {
            let mut adapter = AcpAdapter::new(Vendor::Cursor);
            permission(
                &mut adapter,
                json!({"toolCallId":"shell-one","kind":"execute",
                "rawInput":{"command":"python3 -m unittest -q"}}),
            );
            adapter.approve("1", false).unwrap();
            if evict {
                adapter.session_update(&json!({"sessionUpdate":"tool_call_update","toolCallId":"shell-one",
                    "content":[{"type":"content","content":{"type":"text","text":"x".repeat(TOOL_METADATA_BYTES)}}]}));
            }
            cursor_start(&mut adapter);
            let (_, output) = cursor_terminal(&mut adapter, 0, false);
            assert_eq!(output["acp_provenance"]["history_complete"], false);
        }
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        for i in 0..OBSERVED_TOOL_IDS {
            adapter
                .observed_tool_ids
                .insert(crate::workspace::hash(format!("seen-{i}").as_bytes()));
        }
        cursor_start(&mut adapter);
        let (_, output) = cursor_terminal(&mut adapter, 0, false);
        assert_eq!(output["acp_provenance"]["history_complete"], false);
        assert_eq!(adapter.observed_tool_ids.len(), OBSERVED_TOOL_IDS);
        assert!(adapter.tool_history_exhausted);
        adapter.session_id = Some("session".into());
        adapter.start_prompt("new prompt", &[]).unwrap();
        cursor_start(&mut adapter);
        let (_, output) = cursor_terminal(&mut adapter, 0, false);
        assert_eq!(output["acp_provenance"]["history_complete"], true);
    }

    #[test]
    fn cursor_provenance_rejects_forged_and_cached_terminal_flags_and_bounds_retention() {
        let mut adapter = AcpAdapter::new(Vendor::Cursor);
        cursor_start(&mut adapter);
        cursor_permission(&mut adapter);
        adapter.approve("1", true).unwrap();
        adapter.session_update(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"shell-one",
            "rawOutput":{"exitCode":0,"stdout":"","stderr":"early"}}),
        );
        let output = finished_output(
            &mut adapter,
            json!({"sessionUpdate":"tool_call_update","toolCallId":"shell-one",
            "status":"completed","rawOutput":null,
            "acp_provenance":{"raw_output":{"explicit_terminal":true}},"cursor_execution":{"exit_code":99},
            "content":[{"type":"content","content":{"type":"text","text":"Not in allowlist: python3"}}]}),
        );
        assert_eq!(
            output["acp_provenance"]["raw_output"]["explicit_terminal"],
            false
        );
        assert_eq!(output["acp_provenance"]["content"]["source"], "tool_update");
        assert_eq!(output["acp_provenance"]["content"]["phase"], "completed");
        assert_eq!(output["cursor_execution"]["exit_code"], 0);
        let output = finished_output(
            &mut adapter,
            json!({"sessionUpdate":"tool_call","status":"completed",
            "acp_provenance":{"schema_version":1},"cursor_execution":{"exit_code":99}}),
        );
        assert!(output.get("acp_provenance").is_none());
        assert!(output.get("cursor_execution").is_none());
        for i in 0..TOOL_METADATA_COUNT + 5 {
            adapter.session_update(
                &json!({"sessionUpdate":"tool_call","toolCallId":format!("bounded-{i}"),
                "kind":"execute","rawInput":{"command":"true"}}),
            );
        }
        assert_eq!(adapter.tool_evidence.len(), TOOL_METADATA_COUNT);
        assert!(adapter
            .tool_evidence
            .keys()
            .all(|id| adapter.tool_metadata.contains_key(id)));
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
        let id = adapter
            .pending_permissions
            .keys()
            .filter_map(|key| key.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        adapter.permission_request(&json!(id), &json!({
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
            ToolOrigin::Initial,
        );
        adapter.remember_tool(
            &json!({"toolCallId":"kept","rawInput":{"command":"x".repeat(TOOL_METADATA_BYTES)}}),
            ToolOrigin::Update,
        );
        assert!(
            permission(&mut adapter, json!({"toolCallId":"kept"})).arguments["input"].is_null()
        );
        for i in 0..TOOL_METADATA_COUNT + 5 {
            adapter.remember_tool(
                &json!({"toolCallId":format!("call-{i}"),"rawInput":{"command":"test"}}),
                ToolOrigin::Initial,
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

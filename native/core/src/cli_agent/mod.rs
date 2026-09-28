//! Vendor CLI agent backends: run the user's subscription CLI (Codex, Claude
//! Code, Cursor, Antigravity, and optionally Grok) by spawning the official
//! vendor process. The primary picker features Codex, Claude Code, Cursor, and
//! Antigravity. Grok stays wired but is not featured unless the user already
//! has that CLI.
//!
//! ShadowCode never reads, stores, proxies, or re-implements vendor OAuth
//! tokens. The user logs in once with the vendor CLI (`claude auth login`,
//! `codex login`, `grok login`); ShadowCode only spawns the binary in the
//! trusted workspace and translates its event stream into the existing
//! transcript model. In this mode the VENDOR agent runs the agentic loop with
//! its own tools and sandbox; ShadowCode is the workspace, transcript, review,
//! approval, and steering shell. ShadowCode's native tools and bubblewrap are
//! never injected into the vendor process.
//!
//! Every adapter is a pure line-oriented state machine (`CliAdapter`), so the
//! protocol translation is tested from recorded fixture streams without the
//! real CLIs or logins. `runner` owns the process, stdin/stdout plumbing,
//! approvals, cancellation, and steering.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

pub mod acp;
pub mod acp_probe;
pub mod antigravity_server;
pub mod auth;
pub mod catalog;
pub mod claude;
pub mod claude_probe;
pub mod codex;
pub mod codex_probe;
pub mod discovery;
pub mod doctor;
pub mod handoff;
mod lines;
pub mod picker;
mod probe_lifecycle;
#[cfg(unix)]
pub mod runner;
pub mod usage;

/// Provider prefix used in `ModelConfig.provider` for vendor CLI backends.
pub const PROVIDER_PREFIX: &str = "cli:";
/// Longest accepted NDJSON line from a vendor process. Longer lines are
/// rejected immediately rather than buffered without bound.
pub const MAX_LINE_BYTES: usize = 4_000_000;
/// Consecutive malformed lines tolerated before the run is failed.
pub const MAX_MALFORMED_LINES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Codex,
    Claude,
    Cursor,
    Antigravity,
    Grok,
}
impl Vendor {
    pub const ALL: [Vendor; 5] = [
        Vendor::Codex,
        Vendor::Claude,
        Vendor::Cursor,
        Vendor::Antigravity,
        Vendor::Grok,
    ];
    /// Subscriptions shown in the primary picker.
    pub const FEATURED: [Vendor; 4] = [
        Vendor::Codex,
        Vendor::Claude,
        Vendor::Cursor,
        Vendor::Antigravity,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Vendor::Codex => "codex",
            Vendor::Claude => "claude",
            Vendor::Cursor => "cursor",
            Vendor::Antigravity => "antigravity",
            Vendor::Grok => "grok",
        }
    }
    pub fn provider(self) -> String {
        format!("{PROVIDER_PREFIX}{}", self.id())
    }
    pub fn product_label(self) -> &'static str {
        match self {
            Vendor::Codex => "Codex",
            Vendor::Claude => "Claude Code",
            Vendor::Cursor => "Cursor",
            Vendor::Antigravity => "Antigravity",
            Vendor::Grok => "Grok",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Vendor::Codex => "Codex (vendor agent)",
            Vendor::Claude => "Claude Code (vendor agent)",
            Vendor::Cursor => "Cursor (vendor agent)",
            Vendor::Antigravity => "Antigravity (vendor agent)",
            Vendor::Grok => "Grok (vendor agent)",
        }
    }
    pub fn binary(self) -> &'static str {
        match self {
            Vendor::Codex => "codex",
            Vendor::Claude => "claude",
            Vendor::Cursor => "cursor-agent",
            Vendor::Antigravity => antigravity_server::SERVER_FILE,
            Vendor::Grok => "grok",
        }
    }
    pub fn featured(self) -> bool {
        Self::FEATURED.contains(&self)
    }
    /// Official interfaces that accept image bytes. Grok's ACP `initialize`
    /// reports `promptCapabilities.image: false`; we do not invent a vision
    /// payload for it. The catalog refines this per runtime handshake.
    pub fn accepts_images(self) -> bool {
        matches!(
            self,
            Vendor::Codex | Vendor::Claude | Vendor::Cursor | Vendor::Antigravity
        )
    }
    /// The runtime offers an automatic model choice of its own (Cursor
    /// `default[]`). Codex has a default model but no "auto" router.
    pub fn supports_auto_model(self) -> bool {
        matches!(self, Vendor::Cursor)
    }
    /// Approval prompts from this runtime reach ShadowCode. All runtimes do:
    /// Antigravity runs through its ACP server, which sends
    /// `session/request_permission`.
    pub fn asks_approval(self) -> bool {
        true
    }
    pub fn logout_command(self) -> &'static [&'static str] {
        match self {
            Vendor::Codex => &["logout"],
            Vendor::Claude => &["auth", "logout"],
            Vendor::Cursor => &["logout"],
            Vendor::Grok => &["logout"],
            // Disconnect deletes ShadowCode's private Antigravity profile.
            Vendor::Antigravity => &[],
        }
    }
    /// Shown before Disconnect: the login is shared with the native CLI.
    pub fn shared_cli_note(self) -> String {
        match self {
            Vendor::Antigravity => "Disconnecting deletes ShadowCode's private Antigravity profile and its Google sign-in. The agy CLI and the Antigravity app keep their own sign-in.".into(),
            _ => format!(
                "Disconnecting runs `{} {}`, which signs this account out of the {} CLI everywhere on this computer, not only in ShadowCode.",
                self.binary(),
                self.logout_command().join(" "),
                self.product_label()
            ),
        }
    }
    pub fn login_hint(self) -> &'static str {
        match self {
            Vendor::Codex => "Choose Connect to sign in on OpenAI's page (or run `codex login`).",
            Vendor::Claude => "Choose Connect to sign in on Anthropic's page (or run `claude auth login`).",
            Vendor::Cursor => "Choose Connect to sign in on Cursor's page (or run `cursor-agent login`).",
            Vendor::Antigravity => {
                "Choose Connect to sign in with Google. This sign-in belongs to ShadowCode's Antigravity agent, separate from the agy CLI."
            }
            Vendor::Grok => "Choose Connect to sign in on the Grok page (or run `grok login`).",
        }
    }
    pub fn login_command(self) -> &'static [&'static str] {
        match self {
            Vendor::Codex => &["login"],
            Vendor::Claude => &["auth", "login"],
            Vendor::Cursor => &["login"],
            // Connect runs the ACP server's `authenticate` instead.
            Vendor::Antigravity => &[],
            Vendor::Grok => &["login"],
        }
    }
    pub fn install_hint(self) -> &'static str {
        match self {
            Vendor::Codex => "Install the Codex CLI (`npm i -g @openai/codex`) so `codex` is on PATH.",
            Vendor::Claude => "Install Claude Code so `claude` is on PATH (see https://code.claude.com/docs/en/headless).",
            Vendor::Cursor => "Install Cursor Agent (`cursor-agent`) from https://cursor.com/docs/cli/acp so it is on PATH.",
            Vendor::Antigravity => "Install the Antigravity agent from Settings › Accounts (Google's official ACP server, a 334 MB download from dl.google.com).",
            Vendor::Grok => "Install the Grok CLI so `grok` is on PATH (see https://docs.x.ai/build).",
        }
    }
    pub fn from_provider(provider: &str) -> Option<Vendor> {
        match provider.strip_prefix(PROVIDER_PREFIX)? {
            "codex" => Some(Vendor::Codex),
            "claude" => Some(Vendor::Claude),
            "cursor" => Some(Vendor::Cursor),
            "antigravity" => Some(Vendor::Antigravity),
            "grok" => Some(Vendor::Grok),
            _ => None,
        }
    }
    pub fn parse(id: &str) -> Option<Vendor> {
        Self::from_provider(&format!("{PROVIDER_PREFIX}{id}"))
    }
}

pub fn is_cli_provider(provider: &str) -> bool {
    Vendor::from_provider(provider).is_some()
}

/// A vendor permission prompt routed through ShadowCode's Allow/Deny approvals.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalPrompt {
    /// Opaque protocol-level id the adapter needs to answer the request.
    pub request_id: String,
    /// Internal fixed-size correlation for tool retirement. Kept outside
    /// redacted display arguments and never serialized or accepted from JSON.
    #[serde(skip)]
    pub(super) tool_identity: Option<[u8; 32]>,
    /// `command`, `file_change`, `permissions`, or `tool`.
    pub kind: String,
    /// Tool label shown in the approval card, e.g. `codex.command_execution`.
    pub tool: String,
    /// Human-readable summary (shell command, file list, or tool name).
    pub command: String,
    pub reason: String,
    pub arguments: Value,
}

pub(super) fn approval_tool_identity(id: &str) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(id.as_bytes()).into()
}

/// How ShadowCode answers a vendor permission prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VendorAnswer {
    pub allow: bool,
    /// "Allow for this task": the vendor's own allow-for-session choice,
    /// where the protocol has one.
    pub for_session: bool,
    /// The user's reason for a denial, where the protocol carries one.
    pub note: Option<String>,
}

/// Translated vendor events. These map onto the existing transcript
/// vocabulary (`model.stream`, `tool.started`, `tool.completed`, ...).
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// Streamed assistant text.
    Text(String),
    ToolStarted {
        id: String,
        name: String,
        detail: Value,
    },
    ToolCompleted {
        id: String,
        name: String,
        success: bool,
        output: Value,
    },
    /// Files the vendor agent reported changing.
    FilesChanged { paths: Vec<String>, detail: Value },
    /// A vendor prompt that needs a user decision.
    Approval(ApprovalPrompt),
    /// Non-fatal diagnostic (shown as a warning, never fails the task).
    Warning(String),
    /// Token usage reported by the vendor, when the protocol carries it.
    /// `input` includes `cached` (input served from the vendor's cache).
    Usage {
        input: u64,
        output: u64,
        cached: u64,
    },
    /// The vendor's running cost for this run in US dollars (Claude's
    /// `total_cost_usd`); it replaces, never adds to, an earlier value.
    VendorCost { total_usd: f64 },
    /// The current turn finished. `text` is the final assistant message when
    /// the protocol delivers one that was not already streamed.
    TurnCompleted {
        text: Option<String>,
        interrupted: bool,
    },
    /// The current turn failed; the run ends with this error.
    TurnFailed(String),
    /// The vendor's own session identifier for this conversation, reported
    /// once known so follow-ups can resume it. Kept separate from the
    /// ShadowCode session id.
    NativeSession { id: String },
    /// A plan usage snapshot pushed by the runtime during a turn (Codex
    /// `account/rateLimits/updated`, Claude Code `rate_limit_event`). Never
    /// mixed with token accounting.
    RateLimits(Value),
    /// The account hit its plan limit; the job stops and is not retried.
    LimitReached(String),
}

/// Result of feeding one line into an adapter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Step {
    /// Lines to write to the vendor's stdin, in order, each without newline.
    pub send: Vec<String>,
    pub updates: Vec<Update>,
}
impl Step {
    pub fn send(line: String) -> Self {
        Self {
            send: vec![line],
            updates: Vec::new(),
        }
    }
    pub fn update(update: Update) -> Self {
        Self {
            send: Vec::new(),
            updates: vec![update],
        }
    }
    pub fn merge(mut self, other: Step) -> Self {
        self.send.extend(other.send);
        self.updates.extend(other.updates);
        self
    }
}

/// An image already stored in the workspace. Adapters pass these through
/// official vendor fields (Codex `localImage`, Claude image source blocks,
/// ACP `image` content). They are never reduced to a path list in the prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptImage {
    pub mime: String,
    pub absolute_path: std::path::PathBuf,
    pub data_base64: String,
}

/// A project MCP server the user enabled, shared with a vendor CLI for one
/// run (`mcp::vendor::servers`). Servers that need stored secrets or literal
/// environment values are never shared, so nothing here is sensitive.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpServerSpec {
    /// The definition's name (letters, numbers, `-`, `_`).
    pub name: String,
    /// Program and arguments for a stdio server; empty for HTTP.
    pub command: Vec<String>,
    /// URL of a streamable-HTTP server; empty for stdio.
    pub url: String,
}
impl McpServerSpec {
    /// ACP `McpServer`: stdio always; HTTP only when the agent advertised
    /// `mcpCapabilities.http`.
    pub fn acp(&self, http: bool) -> Option<Value> {
        if let Some((program, args)) = self.command.split_first() {
            Some(json!({"name":self.name,"command":program,"args":args,"env":[]}))
        } else if http && !self.url.is_empty() {
            Some(json!({"type":"http","name":self.name,"url":self.url,"headers":[]}))
        } else {
            None
        }
    }
    /// Claude Code `--mcp-config` JSON (`{"mcpServers":{…}}`).
    pub fn claude_config(servers: &[Self]) -> Option<String> {
        let mut map = serde_json::Map::new();
        for server in servers {
            let value = if let Some((program, args)) = server.command.split_first() {
                json!({"type":"stdio","command":program,"args":args})
            } else if !server.url.is_empty() {
                json!({"type":"http","url":server.url})
            } else {
                continue;
            };
            map.insert(server.name.clone(), value);
        }
        (!map.is_empty()).then(|| json!({"mcpServers": map}).to_string())
    }
    /// Codex `-c mcp_servers.<name>.…=<TOML value>` overrides.
    pub fn codex_overrides(servers: &[Self]) -> Vec<String> {
        let toml_string = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
        let mut args = Vec::new();
        for server in servers {
            if let Some((program, rest)) = server.command.split_first() {
                args.push("-c".into());
                args.push(format!(
                    "mcp_servers.{}.command={}",
                    server.name,
                    toml_string(program)
                ));
                args.push("-c".into());
                args.push(format!(
                    "mcp_servers.{}.args=[{}]",
                    server.name,
                    rest.iter()
                        .map(|a| toml_string(a))
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            } else if !server.url.is_empty() {
                args.push("-c".into());
                args.push(format!(
                    "mcp_servers.{}.url={}",
                    server.name,
                    toml_string(&server.url)
                ));
            }
        }
        args
    }
}

/// Launch options shared by every adapter.
#[derive(Clone, Debug, Default)]
pub struct LaunchOptions {
    pub binary: String,
    pub workspace: std::path::PathBuf,
    /// Vendor model id; empty or `default` keeps the vendor's own default.
    pub model: String,
    /// Plan/review tasks request the vendor's read-only or plan mode where the
    /// protocol offers one.
    pub read_only: bool,
    /// Native session to resume (Codex thread id, ACP session id, Claude
    /// session id, Antigravity conversation id). `None` starts a new one.
    pub resume: Option<String>,
    /// Reasoning effort for the turn (`low`, `medium`, `high`); `None`
    /// keeps the vendor's default. Only Codex and Claude Code take it.
    pub effort: Option<String>,
    /// The installed Claude Code predates its `--effort` flag (the catalog
    /// did not find it in `claude --help`); effort then falls back to the
    /// `MAX_THINKING_TOKENS` thinking budget, which only models without
    /// adaptive thinking honour.
    pub legacy_effort: bool,
    /// Project MCP servers the user enabled, passed to the vendor for this
    /// run (ACP `mcpServers`, Claude `--mcp-config`, Codex `-c mcp_servers`).
    pub mcp_servers: Vec<McpServerSpec>,
}

/// A pure protocol translator. It never touches processes or the network.
pub trait CliAdapter: Send {
    fn vendor(&self) -> Vendor;
    /// Program and arguments to spawn.
    fn command(&self, options: &LaunchOptions) -> (String, Vec<String>);
    /// Lines to write immediately after spawn (handshake).
    fn on_start(&mut self, options: &LaunchOptions) -> Vec<String>;
    /// True once the handshake finished and prompts can be sent.
    fn ready(&self) -> bool;
    /// Queue a user turn. Adapters buffer it until `ready()` and flush it
    /// from `on_line`, so callers may prompt right after `on_start`.
    fn prompt(&mut self, text: &str, images: &[PromptImage]) -> Result<Vec<String>>;
    /// Translate one stdout line. Malformed lines return `Ok` with a
    /// `Warning`; only protocol-fatal conditions return `Err`.
    fn on_line(&mut self, line: &str) -> Result<Step>;
    /// Answer a previously surfaced `ApprovalPrompt`.
    fn approve(&mut self, request_id: &str, approve: bool) -> Result<Vec<String>>;
    /// Answer with the vendor's allow-for-session choice and a denial reason
    /// where the protocol has them; otherwise a plain Allow/Deny.
    fn answer(&mut self, request_id: &str, answer: &VendorAnswer) -> Result<Vec<String>> {
        self.approve(request_id, answer.allow)
    }
    /// A note given with Deny reaches the agent.
    fn deny_note(&self) -> bool {
        false
    }
    /// Extra environment for the vendor process (the thinking budget of a
    /// Claude Code without `--effort`).
    fn env(&self, _options: &LaunchOptions) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Interrupt the running turn (Pause). Empty when unsupported.
    fn interrupt(&mut self) -> Vec<String>;
    /// True when the vendor process is expected to exit on its own after the
    /// final result (one-shot protocols); the runner then waits for exit
    /// instead of terminating the process.
    fn one_shot(&self) -> bool {
        false
    }
    /// The vendor session id currently in use, once the protocol reported it.
    fn native_session(&self) -> Option<String> {
        None
    }
}

/// Construct the adapter for a vendor. `codex_exec_fallback` selects the
/// one-shot `codex exec --json` translator when `codex app-server` is not
/// available.
pub fn adapter_for(vendor: Vendor, codex_exec_fallback: bool) -> Box<dyn CliAdapter> {
    match vendor {
        Vendor::Codex if codex_exec_fallback => Box::new(codex::CodexExecAdapter::default()),
        Vendor::Codex => Box::new(codex::CodexAppServerAdapter::default()),
        Vendor::Claude => Box::new(claude::ClaudeAdapter::default()),
        Vendor::Cursor => Box::new(acp::AcpAdapter::new(Vendor::Cursor)),
        Vendor::Antigravity => Box::new(acp::AcpAdapter::new(Vendor::Antigravity)),
        Vendor::Grok => Box::new(acp::AcpAdapter::new(Vendor::Grok)),
    }
}

/// Per-vendor knobs from `config.cli_agents`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CliAgentsConfig {
    /// Master switch for spawning vendor CLIs.
    pub enabled: bool,
    /// Anthropic tolerates driving the official `claude` binary with the
    /// user's own login but does not guarantee it; this flag lets users opt
    /// out entirely (see docs/NATIVE_CLI_BACKENDS.md).
    pub claude_enabled: bool,
    pub codex_binary: String,
    pub grok_binary: String,
    pub claude_binary: String,
    pub cursor_binary: String,
    pub antigravity_binary: String,
    /// Seconds a vendor approval prompt waits for the user before it is denied.
    pub approval_timeout_sec: u64,
    /// Seconds without any stdout line before the run is considered stalled.
    pub stall_timeout_sec: u64,
    /// Maximum active seconds for one vendor run, even if it keeps producing
    /// output. Explicit approval and paused-steering waits are excluded.
    pub max_run_time_sec: u64,
}
impl Default for CliAgentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            claude_enabled: true,
            codex_binary: "codex".into(),
            grok_binary: "grok".into(),
            claude_binary: "claude".into(),
            cursor_binary: "cursor-agent".into(),
            antigravity_binary: "agy".into(),
            approval_timeout_sec: 600,
            stall_timeout_sec: 900,
            max_run_time_sec: 7200,
        }
    }
}
impl CliAgentsConfig {
    pub fn from_value(value: &Value) -> Result<Self> {
        let config: Self = serde_json::from_value(value.clone())
            .map_err(|e| anyhow::anyhow!("Invalid cli_agents configuration: {e}"))?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("codex_binary", &self.codex_binary),
            ("grok_binary", &self.grok_binary),
            ("claude_binary", &self.claude_binary),
            ("cursor_binary", &self.cursor_binary),
            ("antigravity_binary", &self.antigravity_binary),
        ] {
            if value.trim().is_empty() || value.len() > 1024 || value.contains(['\n', '\0']) {
                bail!("cli_agents.{name} must be a non-empty executable name or path");
            }
        }
        if !(10..=86400).contains(&self.approval_timeout_sec) {
            bail!("cli_agents.approval_timeout_sec must be between 10 and 86400");
        }
        if !(30..=86400).contains(&self.stall_timeout_sec) {
            bail!("cli_agents.stall_timeout_sec must be between 30 and 86400");
        }
        if !(1..=86400).contains(&self.max_run_time_sec) {
            bail!("cli_agents.max_run_time_sec must be between 1 and 86400");
        }
        Ok(())
    }
    pub fn binary(&self, vendor: Vendor) -> &str {
        match vendor {
            Vendor::Codex => &self.codex_binary,
            Vendor::Claude => &self.claude_binary,
            Vendor::Cursor => &self.cursor_binary,
            Vendor::Antigravity => &self.antigravity_binary,
            Vendor::Grok => &self.grok_binary,
        }
    }
    pub fn vendor_enabled(&self, vendor: Vendor) -> bool {
        self.enabled && (vendor != Vendor::Claude || self.claude_enabled)
    }
}

/// Model configuration for a vendor CLI. Empty endpoint, unused key name.
/// ShadowCode never reads a vendor credential for this target. `default`
/// carries the exact picker id (`cli:cursor:gpt-5.5[...]`) so routing
/// decisions and job records keep what the user actually chose.
pub fn vendor_model(vendor: Vendor, model_name: Option<&str>) -> crate::config::ModelConfig {
    let name = model_name
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != vendor.provider())
        .unwrap_or("default")
        .to_owned();
    crate::config::ModelConfig {
        default: picker::target_id(vendor, &name),
        provider: vendor.provider(),
        endpoint: String::new(),
        api_key_env: "UNUSED".into(),
        name,
        context_limit: 200_000,
        keep_alive: "30m".into(),
    }
}

/// Resolve a stable vendor routing id: `cli:<vendor>` or
/// `cli:<vendor>:<exact model id>`. Nothing else is accepted: bare `grok` is
/// the xAI API preset (billed per token), never the Grok CLI, and display
/// labels are not routing ids.
pub fn resolve_vendor(id: &str) -> Option<crate::config::ModelConfig> {
    let rest = id.trim().strip_prefix(PROVIDER_PREFIX)?;
    let (vendor, model) = match rest.split_once(':') {
        Some((vendor, model)) => (vendor, Some(model)),
        None => (rest, None),
    };
    let vendor = Vendor::parse(vendor)?;
    if model.is_some_and(|m| m.trim().is_empty() || m.len() > 512 || m.contains(['\n', '\0'])) {
        return None;
    }
    Some(vendor_model(vendor, model))
}

/// Provider credentials that would silently turn a subscription turn into a
/// pay-per-token API call. They are removed from every vendor CLI child.
pub const API_KEY_VARIABLES: [&str; 10] = [
    // ShadowCode's own OpenRouter key must never reach a vendor CLI either.
    "OPENROUTER_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "CURSOR_API_KEY",
    "XAI_API_KEY",
    "GROK_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
];

/// Remove provider API-key variables from a vendor CLI command so the
/// official CLI uses its own subscription login.
pub fn scrub_api_keys(command: &mut tokio::process::Command) {
    for name in API_KEY_VARIABLES {
        command.env_remove(name);
    }
}

/// Documented plan-limit wording from the vendor runtimes (Codex
/// `usageLimitExceeded`, Claude "usage limit reached", Cursor/Grok quota
/// errors). Used only to stop a job with `limit_reached`; never to retry.
pub fn is_limit_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "usagelimitexceeded",
        "usage limit",
        "usage_limit",
        "rate limit reached",
        "rate_limit_reached",
        "plan limit",
        "quota exceeded",
        "exceeded your quota",
        "out of credits",
        "credits depleted",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Picker rows for enabled vendor backends.
pub fn catalog_models(config: &CliAgentsConfig) -> Vec<Value> {
    Vendor::ALL
        .into_iter()
        .filter(|vendor| config.vendor_enabled(*vendor))
        .map(|vendor| {
            json!({
                "id": picker::target_id(vendor, "default"),
                "name": vendor.product_label(),
                "provider": vendor.provider(),
                "endpoint": "",
                "context_limit": 200000,
                "metadata": {
                    "vendor_agent": true,
                    "kind": vendor.id(),
                    "label": vendor.label(),
                    "product_label": vendor.product_label(),
                    "featured": vendor.featured(),
                    "login_hint": vendor.login_hint(),
                    "group": "subscriptions",
                    "inference": "cloud",
                    "model": "default",
                    "route": "vendor_cli",
                }
            })
        })
        .collect()
}

/// Resolve a vendor to its configured binary, checking PATH when the value is
/// a bare name.
pub fn resolve_binary(configured: &str) -> Option<std::path::PathBuf> {
    let candidate = Path::new(configured);
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(configured))
        .find(|p| p.is_file())
}

/// Truncate vendor output before it enters the transcript or store.
pub(crate) fn clip(text: &str, limit: usize) -> String {
    crate::tools::truncate(text, limit).to_owned()
}

/// Redact what the vendor printed before it is shown or stored.
pub(crate) fn redact(text: &str) -> String {
    crate::redaction::redact_text(text).text
}

pub(crate) fn redact_value(mut value: Value) -> Value {
    crate::redaction::redact_value(&mut value);
    value
}

#[cfg(test)]
mod approval_identity_tests {
    use super::*;

    #[test]
    fn correlation_is_distinct_and_never_serialized_or_deserialized() {
        let first = approval_tool_identity("tool-fixture-ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890");
        assert_ne!(
            first,
            approval_tool_identity("tool-fixture-ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567891")
        );
        let prompt = ApprovalPrompt {
            request_id: "1".into(),
            tool_identity: Some(first),
            kind: "command".into(),
            tool: "cursor.execute".into(),
            command: "true".into(),
            reason: String::new(),
            arguments: serde_json::json!({}),
        };
        let mut encoded = serde_json::to_value(&prompt).unwrap();
        assert!(encoded.get("tool_identity").is_none());
        encoded["tool_identity"] = serde_json::json!(vec![7; 32]);
        let decoded: ApprovalPrompt = serde_json::from_value(encoded).unwrap();
        assert!(decoded.tool_identity.is_none());
    }
}

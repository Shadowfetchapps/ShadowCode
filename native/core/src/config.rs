use crate::paths::{atomic_write, AppPaths};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

// A profile has one owning engine; desktop/CLI requests can still run on
// different threads. Atomic rename alone does not protect read–modify–write.
// These guards cover synchronous local I/O only, never model/network requests.
static CONFIG_UPDATES: Mutex<()> = Mutex::new(());
static SECRET_UPDATES: Mutex<()> = Mutex::new(());
const MAX_CONFIG_BYTES: usize = 1_000_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLevel {
    ReadOnly,
    Workspace,
    Elevated,
}

impl PermissionLevel {
    pub fn restricted_to(self, limit: Self) -> Self {
        match (self, limit) {
            (Self::ReadOnly, _) | (_, Self::ReadOnly) => Self::ReadOnly,
            (Self::Workspace, _) | (_, Self::Workspace) => Self::Workspace,
            _ => Self::Elevated,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub default: String,
    pub provider: String,
    pub endpoint: String,
    pub api_key_env: String,
    pub name: String,
    pub context_limit: usize,
    /// Ollama keep_alive duration string (e.g. "30m", "-1" for indefinite).
    #[serde(default = "default_keep_alive")]
    pub keep_alive: String,
}
impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            default: "mock".into(),
            provider: "mock".into(),
            endpoint: String::new(),
            api_key_env: "OPENAI_API_KEY".into(),
            name: "mock-coder".into(),
            context_limit: 128_000,
            keep_alive: default_keep_alive(),
        }
    }
}

pub fn valid_keep_alive(value: &str) -> bool {
    if matches!(value, "-1" | "0") {
        return true;
    }
    let Some(number) = value
        .strip_suffix("ms")
        .or_else(|| value.strip_suffix('s'))
        .or_else(|| value.strip_suffix('m'))
        .or_else(|| value.strip_suffix('h'))
    else {
        return false;
    };
    number
        .parse::<u32>()
        .is_ok_and(|n| (1..=86400).contains(&n))
}

fn default_keep_alive() -> String {
    "30m".into()
}

/// The two user-facing permission modes.
/// `ask`: file edits and shell commands ask first.
/// `allow_edits`: file edits inside the project are allowed; shell, network
/// and destructive actions still ask (or are denied).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Ask,
    /// Code default keeps pre-0.28 configs working; first-run onboarding
    /// writes `ask` so new installs start restricted.
    #[default]
    AllowEdits,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionsConfig {
    /// User-facing mode. `level` stays as the advanced setting (read_only is
    /// still used by Plan/Review and can be chosen per project).
    pub mode: PermissionMode,
    pub level: PermissionLevel,
    pub require_approval_for_dangerous: bool,
    /// Network-reaching shell commands (curl, npm, ...). Web tools are
    /// governed by `network.mode` and the per-task web flag instead.
    pub network: bool,
    pub allow_root: bool,
    pub profile: String,
    /// Native shell tools require explicit approval unless their exact command
    /// is approved for this task. `ask` mode always asks for shell.
    pub approve_shell: bool,
    /// Runtime only: web tools are allowed for this task (task web flag and
    /// network mode online). Never persisted.
    #[serde(skip)]
    pub web: bool,
    /// Runtime only: `network.mode` is offline. Never persisted.
    #[serde(skip)]
    pub offline: bool,
}
impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            mode: PermissionMode::default(),
            level: PermissionLevel::Workspace,
            require_approval_for_dangerous: true,
            network: false,
            allow_root: false,
            profile: String::new(),
            approve_shell: true,
            web: false,
            offline: false,
        }
    }
}
impl PermissionsConfig {
    /// File edits (write/edit/patch/mkdir/move) ask before running.
    pub fn approve_edits(&self) -> bool {
        self.mode == PermissionMode::Ask
    }
    /// Shell commands ask before running.
    pub fn shell_asks(&self) -> bool {
        self.approve_shell || self.mode == PermissionMode::Ask
    }
    /// Network-reaching shell commands may run (after approval).
    pub fn shell_network(&self) -> bool {
        self.network && !self.offline
    }
}

/// `online`: everything allowed by other settings. `web_off`: web tools are
/// never offered. `offline`: additionally no account/usage refresh or other
/// helper network activity, and cloud routes are unavailable.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    #[default]
    Online,
    WebOff,
    Offline,
}

/// Network for sandboxed shell commands, on top of `permissions.network`
/// (which must be on for `on` and `allowlist`). `allowlist` lets HTTP(S)
/// through a proxy to the hosts in `network.allow` only, and needs bubblewrap.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShellNetwork {
    #[default]
    On,
    Off,
    Allowlist,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    pub mode: NetworkMode,
    /// Shell command network: `on`, `off` or `allowlist` (see [`ShellNetwork`]).
    pub shell: ShellNetwork,
    /// Hosts shell commands may reach in `allowlist` mode: `example.com`,
    /// `*.example.com` (domain and subdomains), `host:port`. Without a port,
    /// 80 and 443.
    pub allow: Vec<String>,
    /// Exact `host:port` entries (for example `localhost:3000`) that web tools
    /// may reach although they are local or use another port.
    pub allow_local_dev: Vec<String>,
    /// Optional SearXNG instance the user runs (base URL). When set,
    /// web_search asks it first (JSON API); DuckDuckGo's page is the fallback.
    pub searxng_url: String,
}
impl NetworkConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.searxng_url.trim().is_empty() {
            let url = reqwest::Url::parse(self.searxng_url.trim())
                .context("network.searxng_url must be an http(s) URL")?;
            ensure!(
                matches!(url.scheme(), "http" | "https") && url.username().is_empty(),
                "network.searxng_url must be an http(s) URL without credentials"
            );
        }
        ensure!(
            self.allow_local_dev.len() <= 32,
            "At most 32 local dev servers can be allowed"
        );
        for entry in &self.allow_local_dev {
            crate::web::normalize_allow_entry(entry)
                .with_context(|| format!("Invalid network.allow_local_dev entry '{entry}'"))?;
        }
        crate::sandbox::proxy::parse_list(&self.allow)?;
        Ok(())
    }
}

/// Old configs had no `permissions.mode`. Map them without widening what they
/// allowed: workspace -> allow_edits (edits were allowed, shell asked);
/// read_only stays read_only; elevated -> allow_edits with shell asking (the
/// level stays elevated, so destructive Git still asks rather than being
/// denied), unless the user had explicitly turned off both approve_shell and
/// require_approval_for_dangerous, in which case their behaviour is kept.
pub fn migrate_permissions(raw: &mut Value) {
    let Some(permissions) = raw.get_mut("permissions").and_then(Value::as_object_mut) else {
        return;
    };
    if permissions.contains_key("mode") {
        return;
    }
    let elevated = permissions.get("level").and_then(Value::as_str) == Some("elevated");
    permissions.insert("mode".into(), json!("allow_edits"));
    if elevated {
        let explicit_off = permissions.get("approve_shell") == Some(&json!(false))
            && permissions.get("require_approval_for_dangerous") == Some(&json!(false));
        if !explicit_off {
            permissions.insert("approve_shell".into(), json!(true));
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub max_steps: usize,
    pub tool_timeout_sec: u64,
    pub parallel_reads: bool,
    pub compact_ratio: f64,
    pub model_retries: usize,
    pub retry_backoff_sec: f64,
    pub max_fix_retries: usize,
    pub max_output_bytes: usize,
    pub max_task_tokens: u64,
    pub autonomy_profile: String,
    /// When history must shrink, ask the current model to summarize the
    /// dropped messages (falls back to the built-in keep-list digest).
    pub summary_compaction: bool,
    /// Longest wait for that summary before falling back.
    pub summary_timeout_sec: u64,
}
impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_steps: 64,
            tool_timeout_sec: 60,
            parallel_reads: true,
            compact_ratio: 0.7,
            model_retries: 3,
            retry_backoff_sec: 1.0,
            max_fix_retries: 3,
            max_output_bytes: 256_000,
            max_task_tokens: 1_000_000,
            autonomy_profile: "normal".into(),
            summary_compaction: true,
            summary_timeout_sec: 60,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub model: ModelConfig,
    pub permissions: PermissionsConfig,
    pub agent: AgentConfig,
    pub ui: Value,
    pub onboarding: Value,
    pub routing: Value,
    pub mcp: Value,
    pub hooks: crate::hooks::HookConfig,
    pub verification: crate::verification::CheckConfig,
    pub git: Value,
    pub logging: Value,
    pub trusted_workspaces: Vec<String>,
    #[serde(default)]
    pub guardian: Value,
    #[serde(default)]
    pub cli_agents: crate::cli_agent::CliAgentsConfig,
    #[serde(default)]
    pub local_engine: crate::local_engine::LocalEngineConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    /// Shell sandbox: `require`, `home_binds`, `landlock`.
    #[serde(default)]
    pub sandbox: crate::sandbox::SandboxConfig,
    /// Workspace checkpoints around shell commands and vendor CLI turns.
    #[serde(default)]
    pub checkpoints: crate::checkpoint::CheckpointConfig,
    /// What happens when a subscription reports its plan limit:
    /// `on_limit` is `"local"` (continue the conversation on a model on this
    /// computer) or `"ask"`; `fallback_model` optionally names the
    /// `local:gguf:` row to use.
    pub limits: Value,
    /// Spending limits for paid API models (never subscriptions or models on
    /// this computer); see [`crate::spending`].
    #[serde(default)]
    pub spending: crate::spending::SpendingConfig,
    /// The daily update check (`updates.check`); see [`crate::updates`].
    #[serde(default)]
    pub updates: crate::updates::UpdatesConfig,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            model: ModelConfig::default(),
            permissions: PermissionsConfig::default(),
            agent: AgentConfig::default(),
            ui: json!({"theme":"light","notify":true,"notify_after_sec":4,"ability":"none"}),
            onboarding: json!({"completed":false,"workspace":""}),
            routing: json!({"enabled":false,"planner":"","coder":"","reviewer":"","tester":""}),
            mcp: json!({"servers":[]}),
            hooks: crate::hooks::HookConfig::default(),
            verification: crate::verification::CheckConfig::default(),
            git: json!({"auto_commit":false,"allow_destructive":false}),
            logging: json!({"level":"info"}),
            trusted_workspaces: Vec::new(),
            guardian: json!({"enabled":false,"interval_sec":3600,"allow_prepare_patch":false}),
            cli_agents: crate::cli_agent::CliAgentsConfig::default(),
            local_engine: crate::local_engine::LocalEngineConfig::default(),
            network: NetworkConfig::default(),
            sandbox: crate::sandbox::SandboxConfig::default(),
            checkpoints: crate::checkpoint::CheckpointConfig::default(),
            limits: json!({"on_limit":"local","fallback_model":""}),
            spending: crate::spending::SpendingConfig::default(),
            updates: crate::updates::UpdatesConfig::default(),
            extra: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn load(paths: &AppPaths, workspace: Option<&Path>) -> Result<Self> {
        let mut base = serde_json::to_value(Self::default())?;
        if paths.config_file().exists() {
            let mut user = read_yaml(&paths.config_file())?;
            migrate_permissions(&mut user);
            merge(&mut base, user);
        }
        let mut config: Self =
            serde_json::from_value(base.clone()).context("Invalid user configuration")?;
        config.validate()?;
        config.apply_runtime(false);
        if let Some(workspace) = workspace {
            let canonical = workspace.canonicalize()?;
            let overlay = canonical.join(".shadow/config/config.yaml");
            if config.is_trusted(&canonical) && overlay.exists() {
                ensure!(
                    overlay.canonicalize()?.starts_with(&canonical),
                    "Project configuration escapes the workspace"
                );
                let project = read_yaml(&overlay)?;
                // Repository files cannot grant permissions, alter credentials,
                // register executable hooks/MCP servers, or redirect model traffic.
                if let Some(agent) = project.get("agent") {
                    merge(&mut base["agent"], agent.clone());
                }
                if project
                    .pointer("/permissions/level")
                    .and_then(Value::as_str)
                    == Some("read_only")
                {
                    base["permissions"]["level"] = json!("read_only");
                }
                config = serde_json::from_value(base)?;
                config.validate()?;
                config.apply_runtime(false);
            }
        }
        config.local_engine.downloads = Some(crate::local_downloads::dir(paths));
        Ok(config)
    }
    /// True when the app must not make helper network requests (account and
    /// usage refresh, model discovery) and cloud routes are unavailable.
    pub fn offline(&self) -> bool {
        self.network.mode == NetworkMode::Offline
    }
    /// Web tools can be offered: the task asked for web and the mode is online.
    pub fn web_tools_allowed(&self, task_web: bool) -> bool {
        task_web && self.network.mode == NetworkMode::Online
    }
    /// Effective network for shell commands: off when the app is offline or
    /// `permissions.network` is off, otherwise `network.shell`.
    pub fn shell_network(&self) -> ShellNetwork {
        if self.permissions.shell_network() {
            self.network.shell
        } else {
            ShellNetwork::Off
        }
    }
    /// Derive the runtime-only permission fields for one task.
    pub fn apply_runtime(&mut self, task_web: bool) {
        self.permissions.offline = self.offline();
        self.permissions.web = self.web_tools_allowed(task_web);
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.ui.is_object(), "UI configuration must be an object");
        ensure!(self.limits.is_object(), "limits must be an object");
        ensure!(
            matches!(
                self.limits["on_limit"].as_str(),
                None | Some("local" | "ask")
            ),
            "limits.on_limit must be \"local\" or \"ask\""
        );
        ensure!(
            self.limits["fallback_model"].is_null()
                || self.limits["fallback_model"].as_str().is_some_and(
                    |m| m.is_empty() || (m.starts_with("local:gguf:") && m.len() <= 1024)
                ),
            "limits.fallback_model must be empty or a local:gguf: model id"
        );
        ensure!(
            valid_keep_alive(&self.model.keep_alive),
            "Ollama residency must be -1, 0, or a positive duration such as 5m, 30m or 1h"
        );
        let guardian: crate::guardian::GuardianConfig =
            serde_json::from_value(self.guardian.clone())
                .context("Invalid Guardian configuration")?;
        ensure!(
            (60..=86400).contains(&guardian.interval_sec),
            "Guardian interval must be between 60 and 86400 seconds"
        );
        ensure!(
            (1024..=4_000_000).contains(&self.model.context_limit),
            "Context limit must be between 1024 and 4000000"
        );
        ensure!(
            (1..=1000).contains(&self.agent.max_steps),
            "Task step limit must be between 1 and 1000"
        );
        ensure!(
            (1..=3600).contains(&self.agent.tool_timeout_sec),
            "Tool timeout must be between 1 and 3600 seconds"
        );
        ensure!(
            self.agent.compact_ratio.is_finite()
                && (0.2..=0.95).contains(&self.agent.compact_ratio),
            "Invalid context compaction ratio"
        );
        ensure!(
            self.agent.model_retries <= 10 && self.agent.max_fix_retries <= 10,
            "Retry limit is too large"
        );
        ensure!(
            self.agent.retry_backoff_sec.is_finite()
                && (0.0..=30.0).contains(&self.agent.retry_backoff_sec),
            "Invalid retry delay"
        );
        ensure!(
            (5..=600).contains(&self.agent.summary_timeout_sec),
            "Compaction summary timeout must be between 5 and 600 seconds"
        );
        ensure!(
            (4096..=4_000_000).contains(&self.agent.max_output_bytes),
            "Invalid tool output limit"
        );
        ensure!(
            self.agent.max_task_tokens > 0,
            "Token budget must be positive"
        );
        ensure!(
            matches!(
                self.agent.autonomy_profile.as_str(),
                "unlimited" | "conservative" | "normal" | "extended" | "custom"
            ),
            "Autonomy profile must be unlimited, conservative, normal, extended, or custom"
        );
        ensure!(
            valid_secret_name(&self.model.api_key_env),
            "Invalid API key environment name"
        );
        if !self.model.endpoint.is_empty() {
            let url =
                reqwest::Url::parse(&self.model.endpoint).context("Invalid model endpoint")?;
            ensure!(
                matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
                "Endpoint must use HTTP or HTTPS"
            );
            ensure!(
                url.username().is_empty() && url.password().is_none(),
                "Use a stored API key instead of credentials in an endpoint URL"
            );
        }
        ensure!(
            matches!(
                self.ui.get("theme").and_then(Value::as_str),
                Some("light" | "dark" | "system")
            ),
            "Unknown theme"
        );
        ensure!(
            self.mcp.get("servers").is_some_and(Value::is_array),
            "MCP servers must be an array"
        );
        #[cfg(unix)]
        crate::mcp::registry::validate_config(&self.mcp)?;
        crate::routing::validate(&self.routing)?;
        self.cli_agents.validate()?;
        self.local_engine.validate()?;
        self.network.validate()?;
        self.sandbox.validate()?;
        self.checkpoints.validate()?;
        self.spending.validate()?;
        self.hooks.validate()?;
        self.verification.validate()?;
        ensure!(
            serde_yaml_ng::to_string(self)?.len() <= MAX_CONFIG_BYTES,
            "Configuration exceeds 1 MB"
        );
        Ok(())
    }
    pub fn is_trusted(&self, workspace: &Path) -> bool {
        self.trusted_workspaces
            .iter()
            .any(|p| same_workspace(Path::new(p.trim()), workspace))
    }
    /// Persist the canonical project path so later job checks match aliases.
    pub fn grant_trust(&mut self, workspace: &Path) {
        let stored = workspace.canonicalize().unwrap_or_else(|_| slim(workspace));
        if stored.as_os_str().is_empty() || self.is_trusted(&stored) {
            return;
        }
        self.trusted_workspaces
            .push(stored.to_string_lossy().into_owned());
    }
    /// Replace the entire configuration. Use update/patch for edits to an
    /// existing profile, so unrelated concurrent changes remain intact.
    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        let _guard = CONFIG_UPDATES
            .lock()
            .map_err(|_| anyhow::anyhow!("Configuration update lock poisoned"))?;
        self.write(paths)
    }
    fn write(&self, paths: &AppPaths) -> Result<()> {
        self.validate()?;
        atomic_write(
            &paths.config_file(),
            serde_yaml_ng::to_string(self)?.as_bytes(),
            true,
        )
    }
    pub fn patch(paths: &AppPaths, values: Value) -> Result<Self> {
        ensure!(values.is_object(), "Configuration patch must be an object");
        Self::update(paths, |config| {
            let mut current = serde_json::to_value(&*config)?;
            merge(&mut current, values);
            *config = serde_json::from_value(current)?;
            Ok(())
        })
    }
    /// Reload, edit, validate and persist without another native writer
    /// intervening. The callback must not call update, patch or save recursively.
    pub fn update(paths: &AppPaths, edit: impl FnOnce(&mut Self) -> Result<()>) -> Result<Self> {
        let _guard = CONFIG_UPDATES
            .lock()
            .map_err(|_| anyhow::anyhow!("Configuration update lock poisoned"))?;
        let mut config = Self::load(paths, None)?;
        edit(&mut config)?;
        config.write(paths)?;
        Ok(config)
    }
}

fn read_yaml(path: &Path) -> Result<Value> {
    let value: Value = serde_yaml_ng::from_str(&read_text(path)?)
        .with_context(|| format!("Invalid YAML in {}", path.display()))?;
    if value.is_null() {
        return Ok(json!({}));
    }
    ensure!(value.is_object(), "Configuration must be a mapping");
    Ok(value)
}

fn read_text(path: &Path) -> Result<String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "Configuration and secrets must be regular files"
    );
    let mut text = String::new();
    file.take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_string(&mut text)?;
    ensure!(
        text.len() <= MAX_CONFIG_BYTES,
        "Configuration or secret file exceeds 1 MB"
    );
    Ok(text)
}

/// True when both paths name the same directory after resolving symlinks,
/// trailing slashes, `.` / `..`, and bind-mount aliases that share an inode.
pub fn same_workspace(left: &Path, right: &Path) -> bool {
    if let (Ok(left), Ok(right)) = (left.canonicalize(), right.canonicalize()) {
        if left == right {
            return true;
        }
        #[cfg(unix)]
        {
            if let (Some(left_id), Some(right_id)) = (file_id(&left), file_id(&right)) {
                return left_id == right_id;
            }
        }
        return false;
    }
    let left = slim(left);
    let right = slim(right);
    !left.as_os_str().is_empty() && left == right
}

#[cfg(unix)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

fn slim(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, overlay) => *base = overlay,
    }
}

pub(crate) fn valid_secret_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn secrets(paths: &AppPaths) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    if !paths.secrets_file().exists() {
        return Ok(result);
    }
    let text = read_text(&paths.secrets_file())?;
    for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if valid_secret_name(key) {
                let value = value.trim();
                let value = if value.starts_with('"') {
                    serde_json::from_str::<String>(value)
                        .unwrap_or_else(|_| value.trim_matches('"').to_owned())
                } else {
                    value.trim_matches('\'').to_owned()
                };
                result.insert(key.into(), value);
            }
        }
    }
    Ok(result)
}

pub fn secret(paths: &AppPaths, name: &str) -> Result<Option<String>> {
    Ok(std::env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .or(secrets(paths)?.remove(name)))
}

pub fn set_secret(paths: &AppPaths, name: &str, value: &str) -> Result<()> {
    ensure!(valid_secret_name(name), "Invalid secret name");
    if value.contains(['\n', '\r', '\0']) || value.len() > 16_384 {
        bail!("Invalid API key");
    }
    let _guard = SECRET_UPDATES
        .lock()
        .map_err(|_| anyhow::anyhow!("Secret update lock poisoned"))?;
    let mut values = secrets(paths)?;
    if value.is_empty() {
        values.remove(name);
    } else {
        values.insert(name.into(), value.into());
    }
    let mut out = String::new();
    for (name, value) in values {
        out.push_str(&format!("{name}={}\n", serde_json::to_string(&value)?));
    }
    ensure!(
        out.len() <= MAX_CONFIG_BYTES,
        "Secret file would exceed 1 MB"
    );
    atomic_write(&paths.secrets_file(), out.as_bytes(), true)
}

/// True when the model route runs on this computer (managed llama.cpp, or an
/// HTTP endpoint on loopback). Vendor CLIs and remote endpoints are cloud
/// routes and are refused in offline mode.
pub fn runs_on_this_computer(model: &ModelConfig) -> bool {
    if model.provider == "llamacpp" {
        return true;
    }
    if crate::cli_agent::is_cli_provider(&model.provider) || model.provider == "mock" {
        return false;
    }
    if model.endpoint.trim().is_empty() {
        return matches!(model.provider.as_str(), "ollama" | "local" | "vllm");
    }
    let Ok(url) = reqwest::Url::parse(&model.endpoint) else {
        return false;
    };
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

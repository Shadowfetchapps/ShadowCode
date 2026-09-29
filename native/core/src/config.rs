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
            "network.allow_local_dev holds at most 32 entries"
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
    /// The daily update check (`updates.check`); see [`crate::updates`].
    #[serde(default)]
    pub updates: crate::updates::UpdatesConfig,
    /// Top-level keys this version does not know (settings from a newer
    /// version, `voice`, `code_intel`, and retired keys such as the old
    /// `git` and `logging` groups) are kept as they are and written back.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            model: ModelConfig::default(),
            permissions: PermissionsConfig::default(),
            agent: AgentConfig::default(),
            // Every key the app reads from the untyped groups has its default
            // here, so `shadowcode config KEY VALUE` can set it and
            // config.example.yaml documents it (tests/config_keys.rs).
            ui: json!({"theme":"light","notify":true,"notify_approval":true,"notify_failed":true,"notify_limit":true,"notify_finished":true,"notify_sound":false}),
            onboarding: json!({"completed":false,"workspace":""}),
            routing: json!({"enabled":false,"planner":"","coder":"","reviewer":"","tester":"","architecture":"","small_edits":"","vision":"","local":""}),
            mcp: json!({"servers":[],"approved":[],"share_with_cli_agents":true}),
            hooks: crate::hooks::HookConfig::default(),
            verification: crate::verification::CheckConfig::default(),
            trusted_workspaces: Vec::new(),
            guardian: json!({"enabled":false,"interval_sec":3600,"allow_prepare_patch":false}),
            cli_agents: crate::cli_agent::CliAgentsConfig::default(),
            local_engine: crate::local_engine::LocalEngineConfig::default(),
            network: NetworkConfig::default(),
            sandbox: crate::sandbox::SandboxConfig::default(),
            checkpoints: crate::checkpoint::CheckpointConfig::default(),
            limits: json!({"on_limit":"local","fallback_model":""}),
            updates: crate::updates::UpdatesConfig::default(),
            extra: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn load(paths: &AppPaths, workspace: Option<&Path>) -> Result<Self> {
        let mut base = serde_json::to_value(Self::default())?;
        let file = paths.config_file();
        let mut user = None;
        if file.exists() {
            let mut value = read_yaml(&file)?;
            migrate_permissions(&mut value);
            merge(&mut base, value.clone());
            user = Some(value);
        }
        let mut config: Self = serde_json::from_value(base.clone())
            .map_err(|error| unreadable(&file, user.as_ref(), error))?;
        config
            .validate()
            .with_context(|| format!("{} needs a fix", file.display()))?;
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
                config = serde_json::from_value(base)
                    .map_err(|error| unreadable(&overlay, Some(&project), error))?;
                config
                    .validate()
                    .with_context(|| format!("{} needs a fix", overlay.display()))?;
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
        ensure!(
            self.ui.is_object(),
            "ui must be a group of settings, for example `ui: {{theme: system}}`"
        );
        ensure!(
            self.limits.is_object(),
            "limits must be a group of settings, for example `limits: {{on_limit: local}}`"
        );
        ensure!(
            matches!(
                self.limits["on_limit"].as_str(),
                None | Some("local" | "ask")
            ),
            "limits.on_limit must be local or ask"
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
            "model.keep_alive must be \"-1\" (keep loaded), \"0\" (unload) or a duration such as \"5m\", \"30m\" or \"1h\" (up to 86400 of a unit); found {:?}",
            self.model.keep_alive
        );
        let guardian: crate::guardian::GuardianConfig =
            serde_json::from_value(self.guardian.clone())
                .context("guardian has a value ShadowCode can't read; use enabled (true/false), interval_sec (seconds) and allow_prepare_patch (true/false)")?;
        ensure!(
            (60..=86400).contains(&guardian.interval_sec),
            "guardian.interval_sec must be between 60 and 86400 seconds; found {}",
            guardian.interval_sec
        );
        ensure!(
            (1024..=4_000_000).contains(&self.model.context_limit),
            "model.context_limit must be between 1024 and 4000000 tokens; found {}",
            self.model.context_limit
        );
        let agent = &self.agent;
        ensure!(
            (1..=1000).contains(&agent.max_steps),
            "agent.max_steps must be between 1 and 1000; found {}",
            agent.max_steps
        );
        ensure!(
            (1..=3600).contains(&agent.tool_timeout_sec),
            "agent.tool_timeout_sec must be between 1 and 3600 seconds; found {}",
            agent.tool_timeout_sec
        );
        ensure!(
            agent.compact_ratio.is_finite() && (0.2..=0.95).contains(&agent.compact_ratio),
            "agent.compact_ratio must be between 0.2 and 0.95; found {}",
            agent.compact_ratio
        );
        ensure!(
            agent.model_retries <= 10,
            "agent.model_retries must be between 0 and 10; found {}",
            agent.model_retries
        );
        ensure!(
            agent.max_fix_retries <= 10,
            "agent.max_fix_retries must be between 0 and 10; found {}",
            agent.max_fix_retries
        );
        ensure!(
            agent.retry_backoff_sec.is_finite() && (0.0..=30.0).contains(&agent.retry_backoff_sec),
            "agent.retry_backoff_sec must be between 0 and 30 seconds; found {}",
            agent.retry_backoff_sec
        );
        ensure!(
            (5..=600).contains(&agent.summary_timeout_sec),
            "agent.summary_timeout_sec must be between 5 and 600 seconds; found {}",
            agent.summary_timeout_sec
        );
        ensure!(
            (4096..=4_000_000).contains(&agent.max_output_bytes),
            "agent.max_output_bytes must be between 4096 and 4000000; found {}",
            agent.max_output_bytes
        );
        ensure!(
            agent.max_task_tokens > 0,
            "agent.max_task_tokens must be greater than 0"
        );
        ensure!(
            matches!(
                agent.autonomy_profile.as_str(),
                "unlimited" | "conservative" | "normal" | "extended" | "custom"
            ),
            "agent.autonomy_profile must be unlimited, conservative, normal, extended or custom; found {:?}",
            agent.autonomy_profile
        );
        ensure!(
            valid_secret_name(&self.model.api_key_env),
            "model.api_key_env must be the name of the variable that holds the key, such as OPENAI_API_KEY (letters, digits and _), never the key itself"
        );
        if !self.model.endpoint.is_empty() {
            let url = reqwest::Url::parse(&self.model.endpoint).with_context(|| {
                format!(
                    "model.endpoint must be a web address such as http://127.0.0.1:11434/v1; found {:?}",
                    self.model.endpoint
                )
            })?;
            ensure!(
                matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
                "model.endpoint must start with http:// or https://"
            );
            ensure!(
                url.username().is_empty() && url.password().is_none(),
                "model.endpoint must not contain a user name or password; remove them and store the API key in Settings instead"
            );
        }
        ensure!(
            matches!(
                self.ui.get("theme").and_then(Value::as_str),
                Some("light" | "dark" | "system")
            ),
            "ui.theme must be system, light or dark; found {}",
            self.ui.get("theme").unwrap_or(&Value::Null)
        );
        ensure!(
            self.mcp.get("servers").is_some_and(Value::is_array),
            "mcp.servers must be a list (use `servers: []` for none)"
        );
        #[cfg(unix)]
        crate::mcp::registry::validate_config(&self.mcp)?;
        crate::routing::validate(&self.routing)?;
        self.cli_agents.validate()?;
        self.local_engine.validate()?;
        self.network.validate()?;
        self.sandbox.validate()?;
        self.checkpoints.validate()?;
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

/// An error for settings serde cannot read, naming the key. The key is found
/// by reading the defaults with one of the user's values at a time.
fn unreadable(file: &Path, user: Option<&Value>, error: serde_json::Error) -> anyhow::Error {
    let fix = "Fix the value, or delete that line to use the default.";
    match user.and_then(failing_key) {
        Some((key, detail)) => anyhow::anyhow!(
            "{}: `{key}` has a value ShadowCode can't read ({detail}). {fix}",
            file.display()
        ),
        None => anyhow::anyhow!(
            "{}: a setting has a value ShadowCode can't read ({error}). {fix}",
            file.display()
        ),
    }
}

/// The dotted path of the first user value that alone makes the settings
/// unreadable, with serde's reason.
pub fn failing_key(user: &Value) -> Option<(String, String)> {
    fn leaves(value: &Value, path: &mut Vec<String>, found: &mut Vec<(Vec<String>, Value)>) {
        match value {
            Value::Object(map) if !map.is_empty() => {
                for (key, child) in map {
                    path.push(key.clone());
                    leaves(child, path, found);
                    path.pop();
                }
            }
            other => found.push((path.clone(), other.clone())),
        }
    }
    let defaults = serde_json::to_value(Config::default()).ok()?;
    let mut found = Vec::new();
    leaves(user, &mut Vec::new(), &mut found);
    for (path, value) in found {
        let overlay = path
            .iter()
            .rev()
            .fold(value, |inner, key| json!({ key.as_str(): inner }));
        let mut probe = defaults.clone();
        merge(&mut probe, overlay);
        if let Err(error) = serde_json::from_value::<Config>(probe) {
            return Some((path.join("."), error.to_string()));
        }
    }
    None
}

fn read_yaml(path: &Path) -> Result<Value> {
    let value: Value = serde_yaml_ng::from_str(&read_text(path)?)
        .with_context(|| format!("Invalid YAML in {}", path.display()))?;
    if value.is_null() {
        return Ok(json!({}));
    }
    ensure!(
        value.is_object(),
        "{} must be a list of `key: value` settings",
        path.display()
    );
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

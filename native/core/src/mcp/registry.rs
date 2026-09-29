//! Inert discovery and explicit per-project/content activation of MCP servers.
use crate::{
    config::{Config, PermissionLevel},
    permissions::{self, Decision},
    workspace::{hash, Workspace},
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};

const DIRECTORIES: &[&str] = &[".shadowcode/mcp", ".shadow/mcp"];
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Definition {
    pub name: String,
    pub command: Option<Vec<String>>,
    pub url: Option<String>,
    /// Bearer token reference; the credential is resolved only on connection.
    pub api_key_env: Option<String>,
    pub description: String,
    /// Retained legacy literal environment, never emitted in catalogs/events.
    pub env: BTreeMap<String, String>,
    /// Child variable -> name in the protected secret store or process environment.
    pub env_refs: BTreeMap<String, String>,
    #[serde(default = "timeout")]
    pub timeout_sec: u64,
}
fn timeout() -> u64 {
    30
}
impl Default for Definition {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: None,
            url: None,
            api_key_env: None,
            description: String::new(),
            env: BTreeMap::new(),
            env_refs: BTreeMap::new(),
            timeout_sec: timeout(),
        }
    }
}
impl Definition {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            crate::workflows::valid_name(&self.name),
            "Invalid MCP server name"
        );
        ensure!(
            self.description.len() <= 1000 && (1..=120).contains(&self.timeout_sec),
            "Invalid MCP description or timeout"
        );
        let command = self.command.as_ref().filter(|v| !v.is_empty());
        let url = self.url.as_ref().filter(|v| !v.is_empty());
        ensure!(
            command.is_some() != url.is_some(),
            "Choose exactly one MCP command or URL"
        );
        if let Some(command) = command {
            ensure!(
                self.api_key_env.is_none(),
                "Bearer credentials require an HTTP server"
            );
            ensure!(
                command.len() <= 128
                    && !command[0].trim().is_empty()
                    && command.iter().all(|s| !s.contains('\0'))
                    && command.iter().map(String::len).sum::<usize>() <= 32_000,
                "Invalid MCP executable or arguments"
            );
        }
        if let Some(url) = url {
            let parsed = super::http::validate_url(url)?;
            ensure!(
                self.env.is_empty() && self.env_refs.is_empty(),
                "HTTP servers use api_key_env; process environment entries require a command"
            );
            if let Some(reference) = &self.api_key_env {
                ensure!(
                    crate::config::valid_secret_name(reference),
                    "Invalid MCP bearer secret reference"
                );
                ensure!(
                    parsed.scheme() == "https" || super::http::loopback(&parsed),
                    "MCP bearer credentials require HTTPS outside loopback"
                );
            }
        }
        ensure!(
            self.env.len() + self.env_refs.len() <= 64,
            "At most 64 MCP environment entries are allowed"
        );
        ensure!(
            self.env
                .iter()
                .all(|(k, v)| crate::config::valid_secret_name(k)
                    && !v.contains('\0')
                    && v.len() <= 16_000),
            "Invalid MCP environment"
        );
        ensure!(
            self.env_refs
                .iter()
                .all(|(k, v)| crate::config::valid_secret_name(k)
                    && crate::config::valid_secret_name(v)
                    && !self.env.contains_key(k)),
            "Invalid or duplicate MCP secret reference"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub workspace: String,
    pub server: String,
    pub hash: String,
}
pub fn activations(config: &Config) -> Result<Vec<Activation>> {
    serde_json::from_value(
        config
            .mcp
            .get("approved")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .context("Invalid MCP activation records")
}
pub fn validate_config(value: &Value) -> Result<()> {
    let servers = value["servers"]
        .as_array()
        .context("mcp.servers must be a list (use `servers: []` for none)")?;
    ensure!(
        servers.len() <= 64 && value.to_string().len() <= 512_000,
        "mcp holds at most 64 servers and 512 KB of settings"
    );
    let approved: Vec<Activation> =
        serde_json::from_value(value.get("approved").cloned().unwrap_or_else(|| json!([])))
            .context("mcp.approved entries must be {workspace, server, hash}")?;
    ensure!(
        approved.len() <= 256,
        "mcp.approved holds at most 256 entries"
    );
    let mut seen = HashSet::new();
    for entry in approved {
        ensure!(
            Path::new(&entry.workspace).is_absolute() && entry.workspace.len() <= 4096,
            "mcp.approved: workspace must be an absolute path"
        );
        validate_id(&entry.server)?;
        ensure!(
            entry.hash.len() == 64 && entry.hash.bytes().all(|c| c.is_ascii_hexdigit()),
            "mcp.approved: hash must be the 64-character hash of the server definition"
        );
        ensure!(
            seen.insert((entry.workspace, entry.server)),
            "mcp.approved lists the same server twice for one project"
        );
    }
    Ok(())
}
fn validate_id(id: &str) -> Result<()> {
    if let Some(name) = id.strip_prefix("config:") {
        ensure!(
            crate::workflows::valid_name(name),
            "Invalid MCP configuration ID"
        );
    } else if let Some(path) = id.strip_prefix("project:") {
        let path = Path::new(path);
        ensure!(
            DIRECTORIES
                .iter()
                .any(|d| path.parent() == Some(Path::new(d)))
                && matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("yaml" | "yml" | "json")
                )
                && path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(crate::workflows::valid_name),
            "Invalid project MCP definition path"
        );
    } else {
        bail!("MCP IDs start with config: or project:");
    }
    Ok(())
}
#[derive(Clone)]
pub struct Entry {
    pub id: String,
    pub hash: String,
    pub definition: Definition,
}
impl Entry {
    pub fn public(&self, enabled: bool) -> Value {
        json!({"id":self.id,"hash":self.hash,"name":self.definition.name,"description":self.definition.description,"command":self.definition.command,"url":self.definition.url,"api_key_env":self.definition.api_key_env,"timeout_sec":self.definition.timeout_sec,"env_names":self.definition.env.keys().collect::<Vec<_>>(),"env_refs":self.definition.env_refs,"enabled":enabled,"transport":if self.definition.command.as_ref().is_some_and(|v| !v.is_empty()) { "stdio" } else { "http" }})
    }
}
pub fn read(workspace: &Workspace, config: &Config, id: &str) -> Result<Entry> {
    validate_id(id)?;
    let (value, content_hash) = if let Some(name) = id.strip_prefix("config:") {
        let matches: Vec<_> = config.mcp["servers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["name"].as_str() == Some(name))
            .collect();
        ensure!(
            matches.len() == 1,
            "MCP configuration is missing or has duplicate server names"
        );
        (matches[0].clone(), hash(matches[0].to_string().as_bytes()))
    } else {
        let file = workspace.read(id.strip_prefix("project:").unwrap())?;
        ensure!(file.content.len() <= 32_000, "MCP definition exceeds 32 KB");
        let mut value: Value = serde_yaml_ng::from_str(&file.content)
            .map_err(|_| anyhow::anyhow!("Invalid MCP YAML/JSON"))?;
        ensure!(value.is_object(), "MCP definition must be an object");
        if value.get("name").is_none() {
            value["name"] = json!(Path::new(&file.path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(""));
        }
        (value, file.hash)
    };
    let definition: Definition = serde_json::from_value(value)
        .map_err(|_| anyhow::anyhow!("Invalid MCP definition fields or types"))?;
    definition.validate()?;
    if id.starts_with("project:") {
        // A file in the repository must not turn a provider or first-party
        // key into a bearer token or child-process variable sent to a URL or
        // command it chose: SECURITY.md promises, for example, that the
        // OpenRouter key reaches only openrouter.ai. A server that needs such
        // a key is added in Settings › Extensions, where the user names it.
        let referenced = definition
            .api_key_env
            .iter()
            .map(String::as_str)
            .chain(definition.env_refs.values().map(String::as_str));
        for name in referenced {
            ensure!(
                !is_protected_secret(name),
                "A project MCP server may not reference {name}; add servers that use provider keys in Settings › Extensions."
            );
        }
    }
    Ok(Entry {
        id: id.into(),
        hash: content_hash,
        definition,
    })
}

/// Provider and first-party credentials a repository file must never point an
/// MCP server at (the vendor-CLI removal list plus ShadowCode's own keys).
fn is_protected_secret(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    crate::cli_agent::API_KEY_VARIABLES.contains(&name.as_str())
        || matches!(
            name.as_str(),
            "GROK_API_KEY"
                | "LLAMA_API_KEY"
                | "SHADOWCODE_NTFY_TOKEN"
                | "NTFY_TOKEN"
                | "OPENROUTER_KEY"
        )
}
pub fn catalog(workspace: &Workspace, config: &Config) -> Result<Value> {
    let approved: Vec<_> = activations(config)?
        .into_iter()
        .filter(|a| a.workspace == workspace.path.to_string_lossy())
        .collect();
    let mut ids = Vec::new();
    let mut issues = Vec::new();
    for server in config.mcp["servers"].as_array().into_iter().flatten() {
        if let Some(name) = server["name"].as_str() {
            ids.push(format!("config:{name}"));
        } else {
            issues.push("A configured MCP entry has no name; edit mcp.servers in settings".into());
        }
    }
    for directory in DIRECTORIES {
        match workspace.list(directory) {
            Ok(files) => {
                for file in files.into_iter().filter(|f| f.kind == "file").take(65) {
                    if ids.len() >= 128 {
                        issues.push("Only the first 128 MCP definitions are displayed".into());
                        break;
                    }
                    if matches!(
                        Path::new(&file.path).extension().and_then(|s| s.to_str()),
                        Some("yaml" | "yml" | "json")
                    ) {
                        ids.push(format!("project:{}", file.path));
                    }
                }
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => issues.push(format!("{directory}: {error:#}")),
        }
    }
    ids.sort();
    ids.dedup();
    let mut entries = Vec::new();
    for id in ids {
        match read(workspace, config, &id) {
            Ok(entry) => entries.push(
                entry.public(
                    approved
                        .iter()
                        .any(|a| a.server == entry.id && a.hash == entry.hash),
                ),
            ),
            Err(error) => issues.push(format!("{id}: {error:#}")),
        }
    }
    Ok(
        json!({"format":"native-mcp-v1","servers":entries,"approved":approved,"issues":issues,"workspace":workspace.path,"trusted":config.is_trusted(&workspace.path),"dirs":DIRECTORIES}),
    )
}
pub fn authorize_start(workspace: &Workspace, config: &Config, entry: &Entry) -> Result<()> {
    ensure!(
        config.is_trusted(&workspace.path),
        "Trust this project before enabling MCP"
    );
    ensure!(
        config.permissions.level != PermissionLevel::ReadOnly,
        "MCP connections are inactive in read-only mode"
    );
    if let Some(url) = entry.definition.url.as_ref().filter(|v| !v.is_empty()) {
        let url = super::http::validate_url(url)?;
        ensure!(
            config.permissions.network || super::http::loopback(&url),
            "Enable network access in permissions before connecting to a remote MCP server"
        );
        return Ok(());
    }
    let command = entry
        .definition
        .command
        .as_ref()
        .filter(|v| !v.is_empty())
        .context("MCP command is missing")?;
    if let Decision::Deny(reason) = permissions::check(
        &config.permissions,
        "exec",
        &json!({"command":command.join(" ")}),
    ) {
        bail!(reason);
    }
    Ok(())
}
pub fn activate(
    workspace: &Workspace,
    config: &mut Config,
    server: &str,
    expected_hash: &str,
    enabled: bool,
) -> Result<()> {
    validate_id(server)?;
    let mut approved = activations(config)?;
    if enabled {
        let entry = read(workspace, config, server)?;
        ensure!(
            entry.hash == expected_hash,
            "MCP definition changed; refresh and review it again"
        );
        authorize_start(workspace, config, &entry)?;
        ensure!(
            approved
                .iter()
                .filter(|a| a.workspace == workspace.path.to_string_lossy() && a.server != server)
                .count()
                < 4,
            "At most four MCP servers may be active in one project"
        );
    }
    approved.retain(|a| a.workspace != workspace.path.to_string_lossy() || a.server != server);
    if enabled {
        approved.push(Activation {
            workspace: workspace.path.to_string_lossy().into(),
            server: server.into(),
            hash: expected_hash.into(),
        });
    }
    config.mcp["approved"] = json!(approved);
    validate_config(&config.mcp)
}
pub fn save_server(config: &mut Config, value: Value, expected_hash: &str) -> Result<()> {
    let definition: Definition = serde_json::from_value(value.clone())
        .map_err(|_| anyhow::anyhow!("Invalid MCP registration fields or types"))?;
    definition.validate()?;
    let servers = config.mcp["servers"]
        .as_array_mut()
        .context("Invalid MCP configuration")?;
    if let Some(existing) = servers.iter_mut().find(|v| v["name"] == definition.name) {
        ensure!(
            hash(existing.to_string().as_bytes()) == expected_hash,
            "MCP server exists or changed; supply its current hash to replace it"
        );
        *existing = value;
    } else {
        ensure!(expected_hash.is_empty(), "MCP server no longer exists");
        servers.push(value);
    }
    validate_config(&config.mcp)
}
pub fn remove_server(config: &mut Config, server: &str, expected_hash: &str) -> Result<()> {
    validate_id(server)?;
    let name = server
        .strip_prefix("config:")
        .context("Project MCP definitions are edited in the project")?;
    let servers = config.mcp["servers"]
        .as_array_mut()
        .context("Invalid MCP configuration")?;
    let index = servers
        .iter()
        .position(|v| v["name"] == name)
        .context("MCP server not found")?;
    ensure!(
        hash(servers[index].to_string().as_bytes()) == expected_hash,
        "MCP server changed; refresh before removing it"
    );
    servers.remove(index);
    // Removing and re-adding identical content requires a fresh activation.
    let mut approved = activations(config)?;
    approved.retain(|a| a.server != server);
    config.mcp["approved"] = json!(approved);
    validate_config(&config.mcp)
}

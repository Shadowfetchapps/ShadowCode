//! Explicitly activated, content-bound project lifecycle commands. Discovery
//! reads confined data only; a repository cannot activate its own executable.
use crate::{
    config::{Config, PermissionLevel},
    events::TaskEvents,
    permissions::{self, Decision},
    process::{self, ProcessSpec},
    tools::truncate,
    workspace::Workspace,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub const EVENTS: &[&str] = &[
    "before_command",
    "after_edit",
    "before_commit",
    "after_test",
    "on_error",
    "on_complete",
    "on_compaction",
];
pub const DIRECTORIES: &[&str] = &[".shadowcode/hooks", ".shadow/hooks"];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HookConfig {
    pub approved: Vec<Approval>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub workspace: String,
    pub path: String,
    pub hash: String,
}
impl HookConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.approved.len() <= 256,
            "hooks.approved holds at most 256 entries"
        );
        let mut seen = HashSet::new();
        for entry in &self.approved {
            ensure!(
                Path::new(&entry.workspace).is_absolute() && entry.workspace.len() <= 4096,
                "hooks.approved: workspace must be an absolute path"
            );
            validate_path(&entry.path)?;
            ensure!(
                entry.hash.len() == 64 && entry.hash.bytes().all(|c| c.is_ascii_hexdigit()),
                "hooks.approved: hash must be the 64-character hash shown by `shadowcode hooks --json`"
            );
            ensure!(
                seen.insert((&entry.workspace, &entry.path)),
                "hooks.approved lists the same hook twice"
            );
        }
        Ok(())
    }
}
fn default_timeout() -> u64 {
    30
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub name: String,
    pub events: Vec<String>,
    pub command: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub path_suffix: String,
    #[serde(default = "default_timeout")]
    pub timeout_sec: u64,
}
impl Definition {
    pub(crate) fn parse(text: &str) -> Result<Self> {
        ensure!(text.len() <= 32000, "Hook definition exceeds 32 KB");
        let value: Self = serde_yaml_ng::from_str(text).context("Invalid hook YAML")?;
        ensure!(
            crate::workflows::valid_name(&value.name),
            "Invalid hook name"
        );
        ensure!(
            !value.command.trim().is_empty()
                && value.command.len() <= 8000
                && !value.command.contains('\0'),
            "Hook command must contain 1–8000 bytes"
        );
        ensure!(
            !value.events.is_empty() && value.events.len() <= EVENTS.len(),
            "Choose one or more lifecycle events"
        );
        let mut seen = HashSet::new();
        for event in &value.events {
            ensure!(
                EVENTS.contains(&event.as_str()) && seen.insert(event),
                "Unknown or duplicate hook event: {event}"
            );
        }
        ensure!(
            (1..=120).contains(&value.timeout_sec),
            "Hook timeout must be 1–120 seconds"
        );
        ensure!(
            value.description.len() <= 1000
                && value.path_suffix.len() <= 80
                && !value.path_suffix.contains(['\0', '\n', '\r']),
            "Hook description or path suffix is invalid"
        );
        Ok(value)
    }
}
fn validate_path(path: &str) -> Result<()> {
    let file = Path::new(path);
    ensure!(
        DIRECTORIES
            .iter()
            .any(|dir| file.parent() == Some(Path::new(dir))),
        "Hooks must be direct files in .shadowcode/hooks or .shadow/hooks"
    );
    ensure!(
        matches!(
            file.extension().and_then(|s| s.to_str()),
            Some("yaml" | "yml" | "json")
        ),
        "Native hooks use YAML or JSON definitions"
    );
    ensure!(
        file.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(crate::workflows::valid_name),
        "Invalid hook filename"
    );
    Ok(())
}
#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    #[serde(flatten)]
    pub definition: Definition,
    pub path: String,
    pub hash: String,
    pub enabled: bool,
    pub builtin: bool,
}
fn read(workspace: &Workspace, path: &str) -> Result<Entry> {
    validate_path(path)?;
    let file = workspace.read(path)?;
    Ok(Entry {
        definition: Definition::parse(&file.content)?,
        path: file.path,
        hash: file.hash,
        enabled: false,
        builtin: false,
    })
}
pub fn catalog(workspace: &Workspace, config: &Config) -> Value {
    let mut entries = Vec::new();
    let mut issues = Vec::new();
    for directory in DIRECTORIES {
        match workspace.list(directory) {
            Ok(files) => {
                for file in files
                    .into_iter()
                    .filter(|file| file.kind == "file")
                    .take(65)
                {
                    if entries.len() + issues.len() >= 64 {
                        issues.push("Only the first 64 hook definitions are displayed".into());
                        break;
                    }
                    if file.name.ends_with(".py") {
                        issues.push(format!("{}: Python register(registry) callbacks need conversion to a native YAML command hook; this file is inactive", file.path));
                    } else if matches!(
                        Path::new(&file.name).extension().and_then(|s| s.to_str()),
                        Some("yaml" | "yml" | "json")
                    ) {
                        match read(workspace, &file.path) {
                            Ok(mut entry) => {
                                entry.enabled = config.hooks.approved.iter().any(|approved| {
                                    approved.workspace == workspace.path.to_string_lossy()
                                        && approved.path == entry.path
                                        && approved.hash == entry.hash
                                });
                                entries.push(entry);
                            }
                            Err(error) => issues.push(format!("{}: {error:#}", file.path)),
                        }
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
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let approved: Vec<_> = config
        .hooks
        .approved
        .iter()
        .filter(|a| a.workspace == workspace.path.to_string_lossy())
        .collect();
    json!({"hooks":entries,"approved":approved,"dirs":DIRECTORIES,"events":EVENTS,"issues":issues,"format":"command-v1","workspace":workspace.path,"trusted":config.is_trusted(&workspace.path)})
}
pub fn activate(
    workspace: &Workspace,
    config: &mut Config,
    path: &str,
    hash: &str,
    enabled: bool,
) -> Result<()> {
    validate_path(path)?;
    if enabled {
        ensure!(
            config.is_trusted(&workspace.path),
            "Trust this project before enabling a lifecycle command"
        );
        let entry = read(workspace, path)?;
        ensure!(
            entry.hash == hash,
            "Hook changed since it was displayed; reload and review it again"
        );
        if let Decision::Deny(reason) = permissions::check(
            &config.permissions,
            "exec",
            &json!({"command":entry.definition.command}),
        ) {
            anyhow::bail!(reason);
        }
        ensure!(
            config
                .hooks
                .approved
                .iter()
                .filter(|e| e.workspace == workspace.path.to_string_lossy() && e.path != path)
                .count()
                < 16,
            "At most 16 hooks may be enabled per project"
        );
    }
    config.hooks.approved.retain(|entry| {
        !(entry.workspace == workspace.path.to_string_lossy() && entry.path == path)
    });
    if enabled {
        config.hooks.approved.push(Approval {
            workspace: workspace.path.to_string_lossy().into_owned(),
            path: path.into(),
            hash: hash.into(),
        });
    }
    config.hooks.validate()
}

#[derive(Clone, Default)]
pub struct Runner {
    entries: Vec<Entry>,
    serial: Arc<tokio::sync::Mutex<()>>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub id: String,
    pub name: String,
    pub event: String,
    pub command: String,
    pub path: String,
    pub hash: String,
    pub status: String,
    pub success: bool,
    pub detail: String,
    pub process: Option<process::ProcessResult>,
}
impl Runner {
    pub fn load(workspace: &Workspace, config: &Config) -> Result<Self> {
        // A read-only task can inspect and repair instructions even when an
        // enabled command's definition is missing or invalid.
        if config.permissions.level == PermissionLevel::ReadOnly
            || !config.is_trusted(&workspace.path)
        {
            return Ok(Self::default());
        }
        let mut entries = Vec::new();
        for approval in config
            .hooks
            .approved
            .iter()
            .filter(|entry| entry.workspace == workspace.path.to_string_lossy())
        {
            ensure!(
                entries.len() < 16,
                "At most 16 hooks may be enabled per project"
            );
            let mut entry = read(workspace, &approval.path).with_context(|| {
                format!(
                    "Enabled hook {} cannot be read; disable it or review a corrected definition",
                    approval.path
                )
            })?;
            ensure!(entry.hash==approval.hash,"Enabled hook {} changed; review and enable its new contents before starting a task",approval.path);
            entry.enabled = true;
            entries.push(entry);
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Self {
            entries,
            ..Self::default()
        })
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub async fn fire(
        &self,
        context: Value,
        workspace: &Workspace,
        config: &Config,
        events: &TaskEvents,
        cancel: CancellationToken,
    ) -> Result<Vec<Outcome>> {
        let event = context["event"].as_str().context("Hook event missing")?;
        ensure!(EVENTS.contains(&event), "Unknown hook event");
        let _serial = tokio::select! {_=cancel.cancelled()=>return Ok(Vec::new()),lock=self.serial.lock()=>lock};
        let mut outcomes = Vec::new();
        for entry in self
            .entries
            .iter()
            .filter(|entry| entry.definition.events.iter().any(|v| v == event))
        {
            if cancel.is_cancelled() {
                break;
            }
            let def = &entry.definition;
            if !def.path_suffix.is_empty()
                && !context["paths"].as_array().is_some_and(|paths| {
                    paths
                        .iter()
                        .any(|p| p.as_str().is_some_and(|p| p.ends_with(&def.path_suffix)))
                })
            {
                continue;
            }
            let mut outcome = Outcome {
                id: crate::id(),
                name: def.name.clone(),
                event: event.into(),
                command: def.command.clone(),
                path: entry.path.clone(),
                hash: entry.hash.clone(),
                status: "running".into(),
                success: true,
                detail: String::new(),
                process: None,
            };
            events.emit("hook.started", json!(outcome))?;
            if config.permissions.level == PermissionLevel::ReadOnly
                || !config.is_trusted(&workspace.path)
            {
                outcome.status = "skipped".into();
                outcome.detail =
                    "Lifecycle commands are inactive in read-only modes and untrusted projects"
                        .into();
            } else {
                let result = async {
                    ensure!(context.to_string().len()<=64000,"Hook context exceeds 64 KB; shorten the command rather than running an incomplete gate");
                    ensure!(
                        workspace.read(&entry.path)?.hash == entry.hash,
                        "Hook definition changed during this task; review it again"
                    );
                    if let Decision::Deny(reason) = permissions::check(
                        &config.permissions,
                        "exec",
                        &json!({"command":def.command}),
                    ) {
                        anyhow::bail!(reason);
                    }
                    // Enabling this exact command is the user's standing approval.
                    // Root/network/read-only restrictions still apply. Context is
                    // passed as data and is never interpolated into shell syntax.
                    let mut spec = ProcessSpec::shell(
                        &def.command,
                        workspace.path.clone(),
                        Duration::from_secs(def.timeout_sec.min(config.agent.tool_timeout_sec)),
                    );
                    spec.output_limit = 16000;
                    spec.env
                        .insert("SHADOW_HOOK_CONTEXT".into(), context.to_string());
                    spec.env.insert("SHADOW_HOOK_EVENT".into(), event.into());
                    spec.env.insert(
                        "SHADOW_HOOK_PATH".into(),
                        context["paths"]
                            .as_array()
                            .and_then(|p| p.first())
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into(),
                    );
                    spec.env.insert(
                        "SHADOW_HOOK_COMMAND".into(),
                        context["command"].as_str().unwrap_or("").into(),
                    );
                    spec.env
                        .insert("SHADOW_HOOK_TASK_ID".into(), events.task_id.clone());
                    spec.env
                        .insert("SHADOW_HOOK_SESSION_ID".into(), events.session_id.clone());
                    process::run(spec, cancel.clone(), None).await
                }
                .await;
                match result {
                    Ok(process) => {
                        outcome.success = process.ok;
                        outcome.status = if process.cancelled {
                            "cancelled"
                        } else if process.ok {
                            "passed"
                        } else {
                            "failed"
                        }
                        .into();
                        outcome.detail = if process.timed_out {
                            "Hook timed out".into()
                        } else {
                            format!("Exited with status {}", process.exit_code)
                        };
                        outcome.process = Some(process);
                    }
                    Err(error) => {
                        outcome.success = false;
                        outcome.status = "failed".into();
                        outcome.detail = format!("{error:#}");
                    }
                }
            }
            // The stored/broadcast event is redacted; the returned outcome
            // keeps the raw process output for the gate decision.
            let mut payload = json!(outcome);
            crate::redaction::redact_value(&mut payload);
            events.emit("hook.completed", payload)?;
            let failed = !outcome.success;
            outcomes.push(outcome);
            // A failing gate prevents both the action and later gate commands.
            if failed && event.starts_with("before_") {
                break;
            }
        }
        Ok(outcomes)
    }
}
pub fn context(event: &str, tool: &str, arguments: &Value, output: &Value, detail: &str) -> Value {
    let mut paths = output["paths"].as_array().cloned().unwrap_or_default();
    if paths.is_empty() {
        if let Some(path) = arguments["path"].as_str() {
            paths.push(json!(path));
        }
    }
    // Operational fields stay exact. An oversized command/context fails closed
    // in the runner instead of presenting a shortened command to a gate.
    json!({"event":event,"tool":tool,"paths":paths,"command":arguments["command"].as_str().unwrap_or(""),"exit_code":output["exit_code"],"detail":truncate(detail,4000),"detail_truncated":detail.len()>4000})
}

pub fn failure(outcomes: &[Outcome]) -> Option<String> {
    let failed: Vec<_> = outcomes
        .iter()
        .filter(|o| !o.success)
        .map(|o| {
            format!(
                "{}: {}{}",
                o.name,
                o.detail,
                o.process
                    .as_ref()
                    .map(|p| format!(
                        "\n{}{}",
                        truncate(&p.stdout, 1000),
                        truncate(&p.stderr, 1000)
                    ))
                    .unwrap_or_default()
            )
        })
        .collect();
    (!failed.is_empty()).then(|| failed.join("\n"))
}
pub fn is_test(command: &str) -> bool {
    let words: Vec<_> = command
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '"' | '\''))
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().any(|w| {
        matches!(
            *w,
            "pytest" | "pytest-3" | "unittest" | "vitest" | "jest" | "ctest"
        )
    }) || words.windows(2).any(|w| {
        matches!(
            w,
            [
                "cargo" | "go" | "npm" | "pnpm" | "yarn" | "bun" | "dotnet",
                "test"
            ]
        )
    })
}

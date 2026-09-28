//! Read-only Claude Code probe over its SDK control protocol.
//!
//! Starts `claude -p --output-format stream-json --input-format stream-json
//! --verbose --strict-mcp-config` and sends one `control_request` with
//! subtype `initialize`, the request Anthropic's Agent SDK sends first. The
//! answer lists the models the signed-in account can use (`value`,
//! `displayName`, `description`, `supportsEffort`, `supportedEffortLevels`)
//! and the account's subscription type. No user message is sent, so no model
//! request is made, no plan allowance is used and no session is saved.
//! `--strict-mcp-config` keeps the user's MCP servers from starting for a
//! status check. Credentials stay with the CLI.
use super::{lines::BoundedLines, MAX_LINE_BYTES};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{ffi::OsStr, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

/// The request id of the probe's `initialize` control request.
pub const INITIALIZE_ID: &str = "shadowcode-initialize";

/// One row of Claude Code's own model picker.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeModel {
    /// What `--model` accepts (`default`, an alias such as `opus`, or a full
    /// model name).
    pub id: String,
    pub label: String,
    /// Claude Code's own default (`value: "default"`).
    pub is_default: bool,
    /// The model takes `--effort` (`supportsEffort`).
    pub effort: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClaudeProbe {
    pub models: Vec<ClaudeModel>,
    /// `account.subscriptionType`, e.g. "Claude Max".
    pub subscription: Option<String>,
    /// `account.email`, shown on the Accounts page like other vendors'.
    pub email: Option<String>,
}

/// Parse the `initialize` control response (`response.response`).
pub fn from_initialize(response: &Value) -> ClaudeProbe {
    let text = |value: &Value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let mut models: Vec<ClaudeModel> = Vec::new();
    for row in response["models"].as_array().into_iter().flatten() {
        let Some(id) = text(&row["value"]).filter(|id| id.len() <= 200) else {
            continue;
        };
        if models.iter().any(|m| m.id == id) {
            continue;
        }
        let label = text(&row["displayName"]).unwrap_or_else(|| id.clone());
        // "Default (recommended)" is the default row's label; the picker
        // already names it Default.
        models.push(ClaudeModel {
            is_default: id == "default",
            effort: row["supportsEffort"].as_bool().unwrap_or(false),
            label,
            id,
        });
    }
    let account = &response["account"];
    ClaudeProbe {
        models,
        subscription: text(&account["subscriptionType"]),
        email: text(&account["email"]).filter(|e| e.contains('@')),
    }
}

fn sanitized(binary: &Path, path_env: Option<&OsStr>, cwd: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .args([
            "-p",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
            "--strict-mcp-config",
        ])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .env_clear()
        .env("NO_COLOR", "1")
        .env("TERM", "dumb");
    for name in [
        "HOME",
        "USER",
        "LANG",
        "LC_ALL",
        "TMPDIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
        "CLAUDE_CONFIG_DIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    match path_env {
        Some(path) => {
            command.env("PATH", path);
        }
        None => {
            if let Some(path) = std::env::var_os("PATH") {
                command.env("PATH", path);
            }
        }
    }
    command
}

/// Ask the installed Claude Code for its model list. An `Err` means the CLI
/// has no SDK `initialize` (older releases) or did not answer in time; the
/// caller then falls back to the aliases in `--help`.
pub async fn probe(
    binary: &Path,
    path_env: Option<&OsStr>,
    deadline: Duration,
) -> Result<ClaudeProbe> {
    if let Some(control) = super::probe_lifecycle::current() {
        return probe_inner(binary, path_env, Some((&control, deadline))).await;
    }
    tokio::time::timeout(deadline, probe_inner(binary, path_env, None))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "claude did not answer initialize within {}s",
                deadline.as_secs()
            )
        })?
}

async fn probe_inner(
    binary: &Path,
    path_env: Option<&OsStr>,
    control: Option<(&super::probe_lifecycle::ProbeControl, Duration)>,
) -> Result<ClaudeProbe> {
    if let Some((control, _)) = control {
        control.check()?;
    }
    let cwd = std::env::temp_dir();
    let mut child = sanitized(binary, path_env, &cwd)
        .spawn()
        .with_context(|| format!("Could not start {}", binary.display()))?;
    let exchange = async {
        let mut stdin = child.stdin.take().context("claude stdin missing")?;
        let stdout = child.stdout.take().context("claude stdout missing")?;
        let mut reader = BoundedLines::new(stdout, MAX_LINE_BYTES);
        let request = json!({
            "type": "control_request",
            "request_id": INITIALIZE_ID,
            "request": {"subtype": "initialize"},
        })
        .to_string();
        stdin.write_all(request.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        loop {
            let Some(line) = reader.next_protocol_line().await? else {
                bail!("claude exited before answering initialize");
            };
            let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if message["type"] != "control_response"
                || message["response"]["request_id"] != INITIALIZE_ID
            {
                continue;
            }
            if message["response"]["subtype"] != "success" {
                bail!(
                    "claude rejected initialize: {}",
                    message["response"]["error"]
                        .as_str()
                        .unwrap_or("unknown error")
                );
            }
            drop(stdin);
            return Ok(from_initialize(&message["response"]["response"]));
        }
    };
    let result = match control {
        Some((control, timeout)) => control.run(timeout, exchange).await,
        None => exchange.await,
    };
    // With stdin closed the CLI exits on its own within about a second;
    // give it that chance to clean up before stopping it.
    if result.is_ok() {
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }
    if control.is_some() {
        super::probe_lifecycle::reap(&mut child).await?;
    } else {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_models_effort_support_and_account() {
        let probe = from_initialize(&json!({
            "models": [
                {"value":"default","displayName":"Default (recommended)","supportsEffort":true},
                {"value":"opus","displayName":"Opus 5.5","supportsEffort":true},
                {"value":"haiku","displayName":"Haiku 4.5"},
                {"value":"opus","displayName":"duplicate"},
                {"value":"","displayName":"no id"},
                {"displayName":"missing"}
            ],
            "account": {"email":"person@example.com","subscriptionType":"Claude Max"}
        }));
        assert_eq!(
            probe
                .models
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["default", "opus", "haiku"]
        );
        assert!(probe.models[0].is_default && probe.models[0].effort);
        assert_eq!(probe.models[1].label, "Opus 5.5");
        assert!(!probe.models[2].effort && !probe.models[2].is_default);
        assert_eq!(probe.subscription.as_deref(), Some("Claude Max"));
        assert_eq!(probe.email.as_deref(), Some("person@example.com"));
        assert_eq!(from_initialize(&json!({})), ClaudeProbe::default());
    }
}

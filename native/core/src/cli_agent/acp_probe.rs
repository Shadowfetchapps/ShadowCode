//! Read-only Agent Client Protocol probe for ACP vendors (Cursor, Grok).
//!
//! Runs the documented handshake — `initialize`, `authenticate` when the agent
//! advertises an auth method, `session/new` — and records what the agent
//! reports about itself: auth methods, prompt capabilities (whether images are
//! accepted on the wire), available modes, and the model catalog. No prompt is
//! sent, so no plan allowance is consumed. The agent keeps its own login; the
//! probe never reads a credential.
use super::{
    lines::{BoundedLines, Line, MAX_DIAGNOSTIC_BYTES},
    MAX_LINE_BYTES,
};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{ffi::OsStr, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

use tokio_util::task::AbortOnDropHandle;

/// A negotiated incompatibility is different from a transient probe failure:
/// neither a login check nor a model-list fallback can make this runtime usable.
#[derive(Debug)]
pub(super) struct UnsupportedProtocolVersion;

impl std::fmt::Display for UnsupportedProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unsupported or missing ACP protocol version; ShadowCode supports version 1. No session was started.")
    }
}

impl std::error::Error for UnsupportedProtocolVersion {}

/// Keep discovery and execution on the same exact supported wire version.
/// In particular, string/float/null declarations do not establish ACP v1.
pub(super) fn require_protocol_v1(
    result: &Value,
) -> std::result::Result<u64, UnsupportedProtocolVersion> {
    match result["protocolVersion"].as_u64() {
        Some(1) => Ok(1),
        _ => Err(UnsupportedProtocolVersion),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AcpModel {
    /// Value accepted by the vendor's `--model` flag / `session/set_model`.
    pub id: String,
    pub label: String,
    pub current: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AcpProbe {
    pub protocol_version: Option<u64>,
    pub agent_version: Option<String>,
    pub auth_methods: Vec<String>,
    /// `authenticate` result: `Some(true)` accepted, `Some(false)` rejected,
    /// `None` when the agent advertised no auth method.
    pub authenticated: Option<bool>,
    pub session_started: bool,
    pub session_error: Option<String>,
    /// `promptCapabilities.image` from `initialize`.
    pub accepts_images: bool,
    pub load_session: bool,
    pub modes: Vec<String>,
    pub models: Vec<AcpModel>,
    pub current_model: Option<String>,
}

fn sanitized(binary: &Path, args: &[&str], workspace: &Path, path_env: Option<&OsStr>) -> Command {
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(path) = path_env {
        command.env("PATH", path);
    }
    // Same environment as a real run: an API key must not make a row look
    // signed in when the task (which also drops it) would not be.
    super::scrub_api_keys(&mut command);
    command
}

fn rpc(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string()
}

/// Model rows from an ACP `modelState`/`models` object. The routing id is
/// the exact `modelId` the agent listed: Cursor only accepts those exact
/// values (with their bracketed parameters) in `session/set_model`, and
/// Grok's ids are plain slugs. Cursor's automatic choice `default[]` is
/// exposed as `auto`.
pub fn models_from_state(state: &Value) -> (Vec<AcpModel>, Option<String>) {
    let current = state["currentModelId"].as_str().map(str::to_owned);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in state["availableModels"].as_array().into_iter().flatten() {
        let model_id = row["modelId"].as_str().unwrap_or("").trim();
        let name = row["name"].as_str().unwrap_or("").trim();
        if model_id.is_empty() {
            continue;
        }
        let auto = model_id == "default[]" || model_id == "default" || model_id == "auto";
        let id = if auto {
            "auto".to_owned()
        } else {
            model_id.to_owned()
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let label = if auto {
            "Auto".to_owned()
        } else if name.is_empty() {
            model_id.split('[').next().unwrap_or(model_id).to_owned()
        } else {
            name.to_owned()
        };
        out.push(AcpModel {
            current: current.as_deref() == Some(model_id),
            id,
            label,
        });
    }
    (out, current)
}

/// Model rows from ACP `configOptions` (Antigravity): the `select` option with
/// id or category `model`. Options may be flat `{value, name}` entries or
/// groups holding `options`.
pub fn models_from_config_options(options: &Value) -> (Vec<AcpModel>, Option<String>) {
    let Some(model) = options.as_array().and_then(|list| {
        list.iter()
            .find(|o| o["id"] == "model" || o["category"] == "model")
    }) else {
        return (Vec::new(), None);
    };
    let current = model["currentValue"].as_str().map(str::to_owned);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |entry: &Value| {
        let Some(value) = entry["value"]
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            return;
        };
        if !seen.insert(value.to_owned()) {
            return;
        }
        let name = entry["name"]
            .as_str()
            .map(str::trim)
            .filter(|n| !n.is_empty());
        out.push(AcpModel {
            id: value.to_owned(),
            label: name.unwrap_or(value).to_owned(),
            current: current.as_deref() == Some(value),
        });
    };
    for entry in model["options"].as_array().into_iter().flatten() {
        if entry["options"].is_array() {
            for inner in entry["options"].as_array().into_iter().flatten() {
                push(inner);
            }
        } else {
            push(entry);
        }
    }
    (out, current)
}

/// Cursor's `default[]` wire value for the automatic model.
pub const CURSOR_AUTO_MODEL_ID: &str = "default[]";

pub async fn probe(
    binary: &Path,
    args: &[&str],
    workspace: &Path,
    path_env: Option<&OsStr>,
    deadline: Duration,
) -> Result<AcpProbe> {
    probe_vendor(binary, args, workspace, path_env, deadline, false).await
}

/// [`probe`]; `antigravity` launches Google's ACP server with ShadowCode's
/// private profile and treats a printed sign-in link as "not signed in".
pub async fn probe_vendor(
    binary: &Path,
    args: &[&str],
    workspace: &Path,
    path_env: Option<&OsStr>,
    deadline: Duration,
    antigravity: bool,
) -> Result<AcpProbe> {
    if let Some(control) = super::probe_lifecycle::current() {
        return probe_inner(
            binary,
            args,
            workspace,
            path_env,
            antigravity,
            Some((&control, deadline)),
        )
        .await;
    }
    tokio::time::timeout(
        deadline,
        probe_inner(binary, args, workspace, path_env, antigravity, None),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "{} did not finish the ACP handshake within {}s",
            binary.display(),
            deadline.as_secs()
        )
    })?
}

async fn probe_inner(
    binary: &Path,
    args: &[&str],
    workspace: &Path,
    path_env: Option<&OsStr>,
    antigravity: bool,
    control: Option<(&super::probe_lifecycle::ProbeControl, Duration)>,
) -> Result<AcpProbe> {
    if let Some((control, _)) = control {
        control.check()?;
    }
    let mut command = sanitized(binary, args, workspace, path_env);
    let _run_dir = if antigravity {
        command.stderr(Stdio::piped());
        let installation = super::antigravity_server::Installation {
            server: binary.to_owned(),
            harness: binary.with_file_name(super::antigravity_server::HARNESS_FILE),
        };
        Some(super::antigravity_server::prepare(
            &mut command,
            &installation,
            false,
        )?)
    } else {
        None
    };
    if let Some((control, _)) = control {
        control.check()?;
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Could not start {}", binary.display()))?;
    // Antigravity prints a Google sign-in link on stderr when it has no
    // valid sign-in, then waits; that answer is "not signed in".
    let (sign_in_tx, mut sign_in_rx) = tokio::sync::oneshot::channel::<()>();
    let stderr_drain = child.stderr.take().map(|stderr| {
        AbortOnDropHandle::new(tokio::spawn(async move {
            let mut lines = BoundedLines::new(stderr, MAX_DIAGNOSTIC_BYTES);
            let mut tx = Some(sign_in_tx);
            while let Ok(Some(line)) = lines.next_line().await {
                let Line::Text(line) = line else {
                    continue;
                };
                if super::antigravity_server::is_sign_in_prompt(&line) {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(());
                    }
                }
            }
        }))
    });
    let exchange = async {
        let mut sign_in_waiting = stderr_drain.is_some();
        let mut stdin = child.stdin.take().context("ACP stdin missing")?;
        let stdout = child.stdout.take().context("ACP stdout missing")?;
        let mut reader = BoundedLines::new(stdout, MAX_LINE_BYTES);
        let mut probe = AcpProbe::default();
        let init = rpc(
            1,
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
                "clientInfo": {"name": "shadowcode", "title": "ShadowCode", "version": crate::VERSION}
            }),
        );
        stdin.write_all(init.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        let mut pending = std::collections::HashSet::from([1u64]);
        let cwd = workspace.display().to_string();
        loop {
            let line = tokio::select! {
                line = reader.next_protocol_line() => line?,
                signal = &mut sign_in_rx, if sign_in_waiting => {
                    // A oneshot cannot be polled again after either delivery or
                    // sender closure (for example stderr reached EOF).
                    sign_in_waiting = false;
                    if signal.is_ok() {
                        probe.authenticated = Some(false);
                        probe.session_error = Some("Google sign-in required".into());
                        break;
                    }
                    continue;
                }
            };
            let Some(line) = line else {
                if pending.is_empty() {
                    break;
                }
                bail!("{} exited during the ACP handshake", binary.display());
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(id) = message.get("id").and_then(Value::as_u64) else {
                continue;
            };
            if message.get("method").is_some() {
                let reply = json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"ShadowCode probe declared no client capabilities"}}).to_string();
                stdin.write_all(reply.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await?;
                continue;
            }
            if !pending.remove(&id) {
                continue;
            }
            let error = message.get("error").filter(|e| !e.is_null()).map(|e| {
                e["data"]["message"]
                    .as_str()
                    .or_else(|| e["message"].as_str())
                    .unwrap_or("unknown error")
                    .to_owned()
            });
            let result = &message["result"];
            match id {
                1 => {
                    if let Some(error) = error {
                        bail!("{} rejected initialize: {error}", binary.display());
                    }
                    // Validate before trusting capabilities or sending authenticate/session/new.
                    probe.protocol_version = Some(require_protocol_v1(result)?);
                    probe.agent_version = result["_meta"]["agentVersion"]
                        .as_str()
                        .or_else(|| result["agentInfo"]["version"].as_str())
                        .map(str::to_owned);
                    probe.accepts_images = result["agentCapabilities"]["promptCapabilities"]
                        ["image"]
                        .as_bool()
                        .unwrap_or(false);
                    probe.load_session = result["agentCapabilities"]["loadSession"]
                        .as_bool()
                        .unwrap_or(false);
                    probe.auth_methods = result["authMethods"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|m| m["id"].as_str().map(str::to_owned))
                        .collect();
                    if let Some(state) = result["_meta"].get("modelState") {
                        let (models, current) = models_from_state(state);
                        if !models.is_empty() {
                            probe.models = models;
                            probe.current_model = current;
                        }
                    }
                    let next = if probe.auth_methods.is_empty() {
                        rpc(3, "session/new", json!({"cwd": cwd, "mcpServers": []}))
                    } else {
                        // Prefer the vendor's own login over a cached-token
                        // method so an expired cache is reported, not assumed.
                        let method = probe
                            .auth_methods
                            .iter()
                            .find(|m| {
                                m.as_str() == "cursor_login"
                                    || m.as_str() == "cached_token"
                                    || (antigravity
                                        && m.as_str() == super::antigravity_server::AUTH_METHOD)
                            })
                            .cloned()
                            .unwrap_or_else(|| probe.auth_methods[0].clone());
                        rpc(2, "authenticate", json!({"methodId": method}))
                    };
                    pending.insert(if probe.auth_methods.is_empty() { 3 } else { 2 });
                    stdin.write_all(next.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await?;
                }
                2 => {
                    probe.authenticated = Some(error.is_none());
                    if let Some(error) = error {
                        probe.session_error = Some(format!("authenticate failed: {error}"));
                        break;
                    }
                    let next = rpc(3, "session/new", json!({"cwd": cwd, "mcpServers": []}));
                    pending.insert(3);
                    stdin.write_all(next.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await?;
                }
                3 => {
                    match error {
                        Some(error) => probe.session_error = Some(error),
                        None => {
                            probe.session_started = result["sessionId"].as_str().is_some();
                            probe.modes = result["modes"]["availableModes"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|m| m["id"].as_str().map(str::to_owned))
                                .collect();
                            let (mut models, mut current) = models_from_state(&result["models"]);
                            if models.is_empty() {
                                (models, current) =
                                    models_from_config_options(&result["configOptions"]);
                            }
                            if !models.is_empty() {
                                probe.models = models;
                                probe.current_model = current;
                            }
                        }
                    }
                    break;
                }
                _ => {}
            }
        }
        drop(stdin);
        Ok(probe)
    };
    let result = match control {
        Some((control, timeout)) => control.run(timeout, exchange).await,
        None => exchange.await,
    };
    if control.is_some() {
        // Retain the drainer handle through cancellation, then join its abort.
        // Dropping it alone would only request abort and could outlive the login.
        if let Some(drain) = stderr_drain {
            drain.abort();
            let _ = drain.await;
        }
        super::probe_lifecycle::reap(&mut child).await?;
    } else if result.is_ok() {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }
    result
}

/// `grok models` prints a login line and a bulleted model list.
pub fn parse_grok_models(text: &str) -> (Option<bool>, Vec<AcpModel>) {
    let mut logged_in = None;
    let mut models = Vec::new();
    let mut default = None;
    for raw in text.lines() {
        let line = raw.trim();
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("you are logged in") {
            logged_in = Some(true);
        } else if lower.contains("not logged in") || lower.starts_with("run `grok login`") {
            logged_in = Some(false);
        } else if let Some(rest) = lower.strip_prefix("default model:") {
            default = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("* ").or_else(|| line.strip_prefix("- ")) {
            let id = rest.split_whitespace().next().unwrap_or("").trim();
            if !id.is_empty() {
                models.push(AcpModel {
                    id: id.to_owned(),
                    label: id.to_owned(),
                    current: false,
                });
            }
        }
    }
    if let Some(default) = default {
        for model in &mut models {
            model.current = model.id == default;
        }
    }
    (logged_in, models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_model_ids_are_exact_wire_values() {
        let state = json!({
            "currentModelId": "default[]",
            "availableModels": [
                {"modelId": "default[]", "name": "Auto"},
                {"modelId": "gpt-5.3-codex[reasoning=medium,fast=false]", "name": "gpt-5.3-codex"},
                {"modelId": "claude-opus-5[thinking=true,context=300k]", "name": "claude-opus-5"}
            ]
        });
        let (models, current) = models_from_state(&state);
        assert_eq!(current.as_deref(), Some("default[]"));
        assert_eq!(models[0].id, "auto");
        assert_eq!(models[0].label, "Auto");
        assert!(models[0].current);
        assert_eq!(models[1].id, "gpt-5.3-codex[reasoning=medium,fast=false]");
        assert_eq!(models[1].label, "gpt-5.3-codex");
        assert_eq!(models[2].label, "claude-opus-5");
    }

    #[test]
    fn grok_models_output_reports_login_and_default() {
        let (logged_in, models) = parse_grok_models(
            "You are logged in with grok.com.\n\nDefault model: grok-4.7\n\nAvailable models:\n  * grok-4.7 (default)\n  - grok-4.6\n",
        );
        assert_eq!(logged_in, Some(true));
        assert_eq!(models.len(), 2);
        assert!(models[0].current);
        assert_eq!(models[1].id, "grok-4.6");
        assert_eq!(
            parse_grok_models("Not logged in. Run `grok login`.").0,
            Some(false)
        );
    }
}

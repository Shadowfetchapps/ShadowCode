//! Read-only Codex app-server probe.
//!
//! Starts `codex app-server` over stdio, performs the documented
//! `initialize` / `initialized` handshake, and asks the official read-only
//! methods for account state (`account/read`), the rate-limit snapshot
//! (`account/rateLimits/read`), and the model catalog (`model/list`). No
//! thread is started, so the probe never consumes plan allowance. Credentials
//! stay with the CLI: the probe only sees what Codex chooses to report.
//!
//! Shapes follow `codex app-server generate-json-schema` (v2 protocol):
//! `Account` is `{type: "chatgpt", email, planType}` or `{type: "apiKey"}`,
//! `RateLimitSnapshot` carries `primary`/`secondary` windows with
//! `usedPercent`, `windowDurationMins`, `resetsAt`, plus `limitId`,
//! `limitName`, `normalModelSlug`, `planType`, `credits`, and
//! `rateLimitsByLimitId` groups snapshots per quota pool.
use super::{lines::BoundedLines, MAX_LINE_BYTES};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{ffi::OsStr, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CodexProbe {
    /// `account/read` result (`account`, `requiresOpenaiAuth`).
    pub account: Option<Value>,
    /// `account/rateLimits/read` result.
    pub rate_limits: Option<Value>,
    /// `model/list` rows.
    pub models: Vec<Value>,
    /// Errors returned by individual requests, keyed by method.
    pub errors: Vec<(String, String)>,
}

impl CodexProbe {
    /// `chatgpt`, `apiKey`, `amazonBedrock`, or `none` when not logged in.
    pub fn auth_mode(&self) -> String {
        self.account
            .as_ref()
            .and_then(|a| a["account"]["type"].as_str())
            .unwrap_or("none")
            .to_owned()
    }
    pub fn logged_in(&self) -> bool {
        self.auth_mode() != "none"
    }
    /// True only for a ChatGPT (subscription) login. An API-key login is
    /// billed per token and must never be presented as plan usage.
    pub fn subscription_login(&self) -> bool {
        self.auth_mode() == "chatgpt"
    }
    pub fn plan_type(&self) -> Option<String> {
        self.account
            .as_ref()
            .and_then(|a| a["account"]["planType"].as_str())
            .map(str::to_owned)
    }
    pub fn email(&self) -> Option<String> {
        self.account
            .as_ref()
            .and_then(|a| a["account"]["email"].as_str())
            .map(str::to_owned)
    }
}

fn sanitized(binary: &Path, path_env: Option<&OsStr>) -> Command {
    let mut command = Command::new(binary);
    command
        .arg("app-server")
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
        "CODEX_HOME",
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

fn rpc(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string()
}

/// Run the probe with an overall deadline. A missing or broken app-server is
/// an `Err`; a healthy server that reports "not logged in" is `Ok` with an
/// empty account.
pub async fn probe(
    binary: &Path,
    path_env: Option<&OsStr>,
    deadline: Duration,
) -> Result<CodexProbe> {
    tokio::time::timeout(deadline, probe_inner(binary, path_env))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "codex app-server did not answer within {}s",
                deadline.as_secs()
            )
        })?
}

async fn probe_inner(binary: &Path, path_env: Option<&OsStr>) -> Result<CodexProbe> {
    let mut child = sanitized(binary, path_env)
        .spawn()
        .with_context(|| format!("Could not start {} app-server", binary.display()))?;
    let mut stdin = child.stdin.take().context("app-server stdin missing")?;
    let stdout = child.stdout.take().context("app-server stdout missing")?;
    let mut reader = BoundedLines::new(stdout, MAX_LINE_BYTES);
    let init = rpc(
        1,
        "initialize",
        json!({"clientInfo":{"name":"shadowcode","title":"ShadowCode","version":crate::VERSION},"capabilities":{"experimentalApi":true}}),
    );
    stdin.write_all(init.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    let mut probe = CodexProbe::default();
    let mut pending = std::collections::HashSet::from([1u64]);
    let mut sent_reads = false;
    loop {
        let Some(line) = reader.next_protocol_line().await? else {
            if pending.is_empty() {
                break;
            }
            bail!("codex app-server exited during the probe");
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            continue; // notifications such as account/updated
        };
        if message.get("method").is_some() {
            // Server-initiated request (e.g. token refresh). Decline: the
            // probe never brokers credentials.
            let reply = json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"ShadowCode probe does not handle server requests"}}).to_string();
            stdin.write_all(reply.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
            continue;
        }
        if !pending.remove(&id) {
            continue;
        }
        let error = message
            .get("error")
            .filter(|e| !e.is_null())
            .map(|e| e["message"].as_str().unwrap_or("unknown error").to_owned());
        match id {
            1 => {
                if let Some(error) = error {
                    bail!("codex app-server rejected initialize: {error}");
                }
                let mut lines = vec![json!({"jsonrpc":"2.0","method":"initialized"}).to_string()];
                lines.push(rpc(2, "account/read", json!({})));
                lines.push(rpc(3, "account/rateLimits/read", json!({})));
                lines.push(rpc(4, "model/list", json!({})));
                for line in lines {
                    stdin.write_all(line.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                }
                stdin.flush().await?;
                pending.extend([2, 3, 4]);
                sent_reads = true;
            }
            2 => match error {
                Some(error) => probe.errors.push(("account/read".into(), error)),
                None => probe.account = Some(message["result"].clone()),
            },
            3 => match error {
                Some(error) => probe.errors.push(("account/rateLimits/read".into(), error)),
                None => probe.rate_limits = Some(message["result"].clone()),
            },
            4 => match error {
                Some(error) => probe.errors.push(("model/list".into(), error)),
                None => {
                    probe.models = message["result"]["data"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                }
            },
            _ => {}
        }
        if sent_reads && pending.is_empty() {
            break;
        }
    }
    drop(stdin);
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    Ok(probe)
}

/// Picker/model rows from `model/list`: id, display name, default flag, and
/// whether the catalog says the model accepts images.
#[derive(Clone, Debug, PartialEq)]
pub struct CodexModel {
    pub id: String,
    pub label: String,
    pub is_default: bool,
    pub vision: bool,
    pub hidden: bool,
}

pub fn models_from_list(rows: &[Value]) -> Vec<CodexModel> {
    rows.iter()
        .filter_map(|row| {
            let id = row["id"].as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            let label = row["displayName"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(id)
                .to_owned();
            let vision = match row["inputModalities"].as_array() {
                Some(modalities) => modalities.iter().any(|m| m == "image"),
                // The schema default is ["text","image"]; a missing field
                // means the catalog did not restrict the model to text.
                None => true,
            };
            Some(CodexModel {
                id: id.to_owned(),
                label,
                is_default: row["isDefault"].as_bool().unwrap_or(false),
                vision,
                hidden: row["hidden"].as_bool().unwrap_or(false),
            })
        })
        .filter(|m| !m.hidden)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_model_list_rows() {
        let rows = vec![
            json!({"id":"gpt-6-astra","displayName":"GPT-6-Astra","isDefault":true,"inputModalities":["text","image"],"hidden":false}),
            json!({"id":"gpt-5.5","displayName":"GPT-5.5","isDefault":false,"inputModalities":["text"]}),
            json!({"id":"secret","displayName":"Hidden","hidden":true}),
            json!({"displayName":"no id"}),
        ];
        let models = models_from_list(&rows);
        assert_eq!(models.len(), 2);
        assert!(models[0].is_default && models[0].vision);
        assert_eq!(models[1].id, "gpt-5.5");
        assert!(!models[1].vision);
    }

    #[test]
    fn account_helpers_distinguish_subscription_from_api_key() {
        let mut probe = CodexProbe::default();
        assert!(!probe.logged_in());
        assert_eq!(probe.auth_mode(), "none");
        probe.account = Some(json!({"account":{"type":"apiKey"}}));
        assert!(probe.logged_in());
        assert!(!probe.subscription_login());
        probe.account =
            Some(json!({"account":{"type":"chatgpt","email":"a@b.c","planType":"pro"}}));
        assert!(probe.subscription_login());
        assert_eq!(probe.plan_type().as_deref(), Some("pro"));
    }
}

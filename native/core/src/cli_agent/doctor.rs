//! Doctor checks for vendor CLI backends. Three honest states per vendor:
//! `not_installed`, `not_logged_in`, `ready` (plus `disabled` when the user
//! switched an adapter off). Login detection only looks at file *presence*
//! (`~/.codex/auth.json`, `~/.grok/auth.json`, `~/.claude/.credentials.json`)
//! or asks the CLI for a boolean (`claude auth status --json` → `loggedIn`).
//! Credential contents are never read into memory, logged, or displayed.
use super::{clip, redact, resolve_binary, CliAgentsConfig, Vendor};
use serde_json::{json, Value};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginState {
    LoggedIn,
    NotLoggedIn,
    Unknown,
}

/// Where a vendor keeps the marker that a login happened. Only existence is
/// checked; the file is never opened.
pub fn auth_marker(vendor: Vendor, home: &Path) -> PathBuf {
    match vendor {
        Vendor::Codex => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"))
            .join("auth.json"),
        Vendor::Grok => home.join(".grok").join("auth.json"),
        Vendor::Claude => std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"))
            .join(".credentials.json"),
        Vendor::Cursor => home.join(".cursor").join("argv.json"),
        Vendor::Antigravity => home.join(".agy").join("session.json"),
    }
}

fn marker_present(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn sanitized_command(
    binary: &Path,
    args: &[&str],
    path_env: Option<&OsStr>,
    home: Option<&Path>,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
        "CLAUDE_CONFIG_DIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if let Some(home) = home {
        // Tests point vendor CLIs at a fixture home; production passes the
        // real one.
        command.env("HOME", home);
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

async fn run_short(
    binary: &Path,
    args: &[&str],
    path_env: Option<&OsStr>,
) -> Option<(bool, String)> {
    run_short_in(binary, args, path_env, None).await
}

async fn run_short_in(
    binary: &Path,
    args: &[&str],
    path_env: Option<&OsStr>,
    home: Option<&Path>,
) -> Option<(bool, String)> {
    run_short_limited(binary, args, path_env, home, 4000).await
}

async fn run_short_limited(
    binary: &Path,
    args: &[&str],
    path_env: Option<&OsStr>,
    home: Option<&Path>,
    limit: usize,
) -> Option<(bool, String)> {
    let output = if let Some(control) = super::probe_lifecycle::current() {
        control.check().ok()?;
        let mut child = sanitized_command(binary, args, path_env, home)
            .spawn()
            .ok()?;
        // Keep the child outside the cancellable exchange. Pipes and buffers
        // live inside it, so cancellation closes our pipe ends without tasks.
        let exchange = async {
            use anyhow::Context;
            use tokio::io::AsyncReadExt;
            let mut stdout = child
                .stdout
                .take()
                .context("Account-check stdout missing")?;
            let mut stderr = child
                .stderr
                .take()
                .context("Account-check stderr missing")?;
            let mut out = Vec::new();
            let mut err = Vec::new();
            let (status, _, _) = tokio::try_join!(
                child.wait(),
                stdout.read_to_end(&mut out),
                stderr.read_to_end(&mut err)
            )?;
            Ok(std::process::Output {
                status,
                stdout: out,
                stderr: err,
            })
        };
        let result = control.run(Duration::from_secs(8), exchange).await;
        super::probe_lifecycle::reap(&mut child).await.ok()?;
        result.ok()?
    } else {
        tokio::time::timeout(
            Duration::from_secs(8),
            sanitized_command(binary, args, path_env, home).output(),
        )
        .await
        .ok()?
        .ok()?
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stderr).into_owned();
    }
    Some((output.status.success(), clip(&text, limit).to_owned()))
}

/// Redacted, clipped text of a short read-only command (stdout, else stderr).
pub async fn short_text(binary: &Path, args: &[&str], path_env: Option<&OsStr>) -> Option<String> {
    let (_, text) = run_short(binary, args, path_env).await?;
    Some(redact(&text))
}

/// `<binary> --help`, for feature detection of documented flags. Kept
/// whole (up to 64 KB): Claude Code's help is about 22 KB and documents
/// `--effort` and the `--model` aliases past the first 4 KB.
pub async fn help_text(binary: &Path, path_env: Option<&OsStr>) -> Option<String> {
    let (_, text) = run_short_limited(binary, &["--help"], path_env, None, 64 * 1024).await?;
    Some(redact(&text))
}

/// Documented `codex login status`: exit 0 means signed in. The output is a
/// sentence such as "Logged in using ChatGPT"; no credential is printed.
pub async fn codex_login_status(binary: &Path, path_env: Option<&OsStr>) -> LoginState {
    codex_login_status_in(binary, path_env, None).await
}

async fn codex_login_status_in(
    binary: &Path,
    path_env: Option<&OsStr>,
    home: Option<&Path>,
) -> LoginState {
    match run_short_in(binary, &["login", "status"], path_env, home).await {
        Some((true, _)) => LoginState::LoggedIn,
        Some((false, text)) => {
            let lower = text.to_ascii_lowercase();
            if lower.contains("not logged in") || lower.contains("not signed in") {
                LoginState::NotLoggedIn
            } else {
                LoginState::Unknown
            }
        }
        None => LoginState::Unknown,
    }
}

/// `claude auth status --json` reports a boolean without secrets.
pub async fn claude_login_state(binary: &Path, path_env: Option<&OsStr>) -> LoginState {
    claude_login_state_in(binary, path_env, None).await
}

async fn claude_login_state_in(
    binary: &Path,
    path_env: Option<&OsStr>,
    home: Option<&Path>,
) -> LoginState {
    claude_auth_status_in(binary, path_env, home).await.0
}

/// `claude auth status --json`: login state plus `authMethod`
/// (`claude.ai` for a subscription login; anything else is billed per token).
pub async fn claude_auth_status(
    binary: &Path,
    path_env: Option<&OsStr>,
) -> (LoginState, Option<String>) {
    claude_auth_status_in(binary, path_env, None).await
}

async fn claude_auth_status_in(
    binary: &Path,
    path_env: Option<&OsStr>,
    home: Option<&Path>,
) -> (LoginState, Option<String>) {
    match run_short_in(binary, &["auth", "status", "--json"], path_env, home).await {
        Some((_, text)) => {
            // The CLI pretty-prints the JSON over several lines.
            let parsed: Option<Value> = serde_json::from_str(text.trim()).ok().or_else(|| {
                text.lines()
                    .find_map(|line| serde_json::from_str(line.trim()).ok())
            });
            let method = parsed
                .as_ref()
                .and_then(|v| v["authMethod"].as_str())
                .map(str::to_owned);
            let state = match parsed.and_then(|v| v["loggedIn"].as_bool()) {
                Some(true) => LoginState::LoggedIn,
                Some(false) => LoginState::NotLoggedIn,
                None => LoginState::Unknown,
            };
            (state, method)
        }
        None => (LoginState::Unknown, None),
    }
}

/// `grok models`: login line plus the model list.
pub async fn grok_models(
    binary: &Path,
    path_env: Option<&OsStr>,
) -> Option<(bool, Vec<super::acp_probe::AcpModel>)> {
    grok_models_in(binary, path_env, None).await
}

async fn grok_models_in(
    binary: &Path,
    path_env: Option<&OsStr>,
    home: Option<&Path>,
) -> Option<(bool, Vec<super::acp_probe::AcpModel>)> {
    let (ok, text) = run_short_in(binary, &["models"], path_env, home).await?;
    let (logged_in, models) = super::acp_probe::parse_grok_models(&text);
    Some((logged_in.unwrap_or(ok && !models.is_empty()), models))
}

/// Version string from `<binary> --version`, first line only, redacted.
pub async fn version(binary: &Path, path_env: Option<&OsStr>) -> Option<String> {
    let (_, text) = run_short(binary, &["--version"], path_env).await?;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(clip(&redact(line), 120).to_owned())
}

/// Login state without touching credential contents.
pub async fn login_state(
    vendor: Vendor,
    binary: &Path,
    home: &Path,
    path_env: Option<&OsStr>,
) -> LoginState {
    // A credential marker file is never proof of a live login; every vendor
    // is asked through its own documented status command. The marker only
    // short-circuits the obvious "never signed in" case for Codex/Grok so the
    // CLI is not started needlessly.
    if matches!(vendor, Vendor::Codex | Vendor::Grok) && !marker_present(&auth_marker(vendor, home))
    {
        // Codex may also hold a login through the app-server keyring; ask it.
        if vendor == Vendor::Grok {
            return LoginState::NotLoggedIn;
        }
    }
    match vendor {
        // Claude Code may keep credentials in the OS keychain instead of a
        // file; its status command reports a boolean without secrets.
        Vendor::Claude => claude_login_state_in(binary, path_env, Some(home)).await,
        Vendor::Codex => codex_login_status_in(binary, path_env, Some(home)).await,
        Vendor::Grok => match grok_models_in(binary, path_env, Some(home)).await {
            Some((true, _)) => LoginState::LoggedIn,
            Some((false, _)) => LoginState::NotLoggedIn,
            None => LoginState::Unknown,
        },
        Vendor::Cursor => match run_short(binary, &["status"], path_env).await {
            Some((_, text)) => {
                let lower = text.to_ascii_lowercase();
                if lower.contains("logged in") {
                    LoginState::LoggedIn
                } else if lower.contains("not logged") || lower.contains("unauthenticated") {
                    LoginState::NotLoggedIn
                } else {
                    LoginState::Unknown
                }
            }
            None => LoginState::Unknown,
        },
        Vendor::Antigravity => match run_short(binary, &["models"], path_env).await {
            Some((ok, text)) => {
                let lower = text.to_ascii_lowercase();
                if ok
                    && (text.contains('\t') || lower.contains("gemini") || lower.contains("claude"))
                {
                    LoginState::LoggedIn
                } else if lower.contains("login")
                    || lower.contains("auth")
                    || lower.contains("sign in")
                {
                    LoginState::NotLoggedIn
                } else if ok {
                    LoginState::LoggedIn
                } else {
                    LoginState::Unknown
                }
            }
            None => LoginState::Unknown,
        },
    }
}

/// One doctor check for a vendor. `home` and `path_env` are injectable so the
/// three states are testable without the real CLIs.
pub async fn check_vendor(
    vendor: Vendor,
    config: &CliAgentsConfig,
    home: &Path,
    path_env: Option<&OsStr>,
) -> Value {
    let id = format!("cli-{}", vendor.id());
    let label = format!("{} via `{}` CLI", vendor.label(), vendor.binary());
    if !config.vendor_enabled(vendor) {
        return json!({
            "id": id, "status": "info", "label": label, "state": "disabled",
            "detail": "Disabled in Settings → Advanced (cli_agents); ShadowCode will not spawn this CLI",
            "fix": "",
        });
    }
    let configured = config.binary(vendor);
    let Some(binary) = resolve_with_path(configured, path_env) else {
        return json!({
            "id": id, "status": "info", "label": label, "state": "not_installed",
            "detail": format!("Not installed: `{configured}` was not found on PATH"),
            "fix": vendor.install_hint(),
        });
    };
    let version = version(&binary, path_env).await;
    let login = login_state(vendor, &binary, home, path_env).await;
    let version_text = version.clone().unwrap_or_else(|| "version unknown".into());
    match login {
        LoginState::LoggedIn => json!({
            "id": id, "status": "pass", "label": label, "state": "ready",
            "detail": format!("Ready: {version_text}; login detected (credential contents are never read or shown)"),
            "fix": "",
            "binary": binary, "version": version,
        }),
        LoginState::NotLoggedIn => json!({
            "id": id, "status": "warn", "label": label, "state": "not_logged_in",
            "detail": format!("Installed ({version_text}) but not logged in"),
            "fix": vendor.login_hint(),
            "binary": binary, "version": version,
        }),
        LoginState::Unknown => json!({
            "id": id, "status": "warn", "label": label, "state": "not_logged_in",
            "detail": format!("Installed ({version_text}); login state could not be determined"),
            "fix": vendor.login_hint(),
            "binary": binary, "version": version,
        }),
    }
}

fn resolve_with_path(configured: &str, path_env: Option<&OsStr>) -> Option<PathBuf> {
    match path_env {
        None => resolve_binary(configured),
        Some(path) => {
            let candidate = Path::new(configured);
            if candidate.components().count() > 1 {
                return candidate.is_file().then(|| candidate.to_path_buf());
            }
            std::env::split_paths(path)
                .map(|dir| dir.join(configured))
                .find(|p| p.is_file())
        }
    }
}

/// All three vendor checks for the running user.
pub async fn checks(config: &CliAgentsConfig) -> Vec<Value> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    let mut out = Vec::new();
    for vendor in Vendor::ALL {
        out.push(check_vendor(vendor, config, &home, None).await);
    }
    out
}

/// Compact status for the UI picker chip: `{vendor: {state, detail, version}}`.
pub async fn status(config: &CliAgentsConfig) -> Value {
    let mut map = serde_json::Map::new();
    for check in checks(config).await {
        if let Some(vendor) = check["id"].as_str().and_then(|id| id.strip_prefix("cli-")) {
            map.insert(
                vendor.to_owned(),
                json!({"state":check["state"],"status":check["status"],"detail":check["detail"],"version":check["version"],"fix":check["fix"]}),
            );
        }
    }
    Value::Object(map)
}

#[cfg(all(test, target_os = "linux"))]
mod login_lifecycle_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn controlled_short_command_is_reaped_before_cancel_returns() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("version-fixture");
        let pid_file = temp.path().join("entered");
        std::fs::write(
            &script,
            b"#!/usr/bin/env python3\nimport os, pathlib, sys, time\npathlib.Path(sys.argv[1]).write_text(str(os.getpid()))\ntime.sleep(30)\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cancel = CancellationToken::new();
        let pid_arg = pid_file.to_str().unwrap();
        let args = [pid_arg];
        let command = super::super::probe_lifecycle::scope(
            cancel.clone(),
            // Generous: a loaded CI host can take seconds to start Python.
            // The barrier below stays well inside this deadline.
            tokio::time::Instant::now() + Duration::from_secs(20),
            short_text(&script, &args, None),
        );
        let interrupt = async {
            let entered = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if let Ok(text) = std::fs::read_to_string(&pid_file) {
                        if let Ok(pid) = text.parse::<u32>() {
                            let identity =
                                std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
                            if let Some(identity) = identity {
                                break (pid, identity);
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await;
            // Always cancel before asserting fixture readiness, so an early
            // setup failure cannot leave a deliberately held child behind.
            cancel.cancel();
            entered
        };
        let (result, entered) = tokio::join!(command, interrupt);
        let (pid, _identity) =
            entered.expect("private version probe reached its live-child barrier");
        assert!(
            result.is_none(),
            "cancelled read cannot publish version output"
        );
        // No grace period or sleep after return: a zombie also fails this.
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "owned short-command child was not reaped at return"
        );
    }
}

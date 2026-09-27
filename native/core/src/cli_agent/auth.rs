//! Official sign-in and sign-out for subscription CLIs.
//!
//! Connect runs the vendor's own login command (`codex login`,
//! `claude auth login`, `cursor-agent login`, `grok login`) as a supervised
//! child with the user's environment (minus provider API keys), so the vendor
//! opens the browser or prints a URL / device code itself. ShadowCode never
//! sees a password or token: it only relays the lines the CLI prints,
//! redacted, as `account.login {vendor, line, url}` events, and reports the
//! exit as `account.login.done {vendor, ok, detail}`. One login per vendor
//! runs at a time; it can be cancelled and stops after `LOGIN_TIMEOUT`.
//!
//! Disconnect runs the official logout command, which signs the CLI out for
//! the whole user account (shared with terminal use), then forgets cached
//! status, persisted usage, and stored native session ids for that vendor.
use super::{catalog::VendorCatalog, clip, redact, resolve_binary, CliAgentsConfig, Vendor};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const LOGOUT_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_LOGIN_LINES: usize = 200;
const LOGIN_DRAIN_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Default)]
struct LoginSession {
    started_at: f64,
    lines: Vec<Value>,
    done: Option<Value>,
    cancel: CancellationToken,
}

/// In-memory login sessions, one per vendor. Dropping it (engine shutdown)
/// cancels every running login, which stops the login child.
#[derive(Default)]
pub struct Logins {
    sessions: Mutex<HashMap<Vendor, LoginSession>>,
}

impl Logins {
    fn begin(&self, vendor: Vendor) -> Option<CancellationToken> {
        let mut sessions = self.sessions.lock().ok()?;
        if sessions.get(&vendor).is_some_and(|s| s.done.is_none()) {
            return None;
        }
        let cancel = CancellationToken::new();
        sessions.insert(
            vendor,
            LoginSession {
                started_at: crate::now(),
                cancel: cancel.clone(),
                ..Default::default()
            },
        );
        Some(cancel)
    }
    fn push(&self, vendor: Vendor, line: Value) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(session) = sessions.get_mut(&vendor) {
                if session.lines.len() >= MAX_LOGIN_LINES {
                    session.lines.remove(0);
                }
                session.lines.push(line);
            }
        }
    }
    fn finish(&self, vendor: Vendor, done: Value) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(session) = sessions.get_mut(&vendor) {
                session.done = Some(done);
            }
        }
    }
    /// `{running, started_at, lines, done}` for the Accounts page.
    pub fn status(&self, vendor: Vendor) -> Value {
        let Ok(sessions) = self.sessions.lock() else {
            return json!({"running":false});
        };
        match sessions.get(&vendor) {
            Some(session) => json!({
                "vendor": vendor.id(),
                "running": session.done.is_none(),
                "started_at": session.started_at,
                "lines": session.lines,
                "done": session.done,
            }),
            None => json!({"vendor":vendor.id(),"running":false,"lines":[],"done":null}),
        }
    }
    pub fn running(&self, vendor: Vendor) -> bool {
        self.sessions
            .lock()
            .map(|s| s.get(&vendor).is_some_and(|s| s.done.is_none()))
            .unwrap_or(false)
    }
    /// Cancel a running login. Returns false when none was running.
    pub fn cancel(&self, vendor: Vendor) -> bool {
        let Ok(sessions) = self.sessions.lock() else {
            return false;
        };
        match sessions.get(&vendor) {
            Some(session) if session.done.is_none() => {
                session.cancel.cancel();
                true
            }
            _ => false,
        }
    }
    pub fn cancel_all(&self) {
        if let Ok(sessions) = self.sessions.lock() {
            for session in sessions.values() {
                session.cancel.cancel();
            }
        }
    }
}

impl Drop for Logins {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

/// First https URL in a printed line, unless it carries a credential-looking
/// query parameter (an OAuth callback with `code=` or a token).
pub fn login_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | ')'))
        .collect();
    let parsed = reqwest::Url::parse(url.trim_end_matches(['.', ','])).ok()?;
    let sensitive = parsed.query_pairs().any(|(key, _)| {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "code" | "token" | "access_token" | "id_token" | "refresh_token" | "api_key"
        )
    });
    (!sensitive).then(|| parsed.to_string())
}

fn login_command(binary: &Path, args: &[&str]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("NO_COLOR", "1");
    // The user's environment (browser, display, config dirs) so the vendor
    // flow works as in a terminal, minus provider API keys.
    // No separate process group: a browser the CLI launches must survive
    // when the login child is stopped.
    super::scrub_api_keys(&mut command);
    #[cfg(target_os = "linux")]
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

/// Start the official login flow. Returns immediately with
/// `{ok, state: started|already_running|unsupported, note, hint?}`.
pub async fn connect(
    catalog: &Arc<VendorCatalog>,
    vendor: Vendor,
    config: &CliAgentsConfig,
) -> Result<Value> {
    connect_with_timeout(catalog, vendor, config, LOGIN_TIMEOUT).await
}

/// `connect` with an explicit time limit for the login child.
pub async fn connect_with_timeout(
    catalog: &Arc<VendorCatalog>,
    vendor: Vendor,
    config: &CliAgentsConfig,
    timeout: Duration,
) -> Result<Value> {
    if vendor == Vendor::Antigravity {
        return connect_antigravity(catalog, config, timeout).await;
    }
    if vendor.login_command().is_empty() {
        return Ok(json!({
            "ok": false,
            "state": "unsupported",
            "note": format!("{} has no sign-in command ShadowCode can run.", vendor.product_label()),
        }));
    }
    if !config.vendor_enabled(vendor) {
        bail!(
            "{} is disabled in Settings › Advanced",
            vendor.product_label()
        );
    }
    let configured = config.binary(vendor);
    let binary = resolve_binary(configured)
        .with_context(|| format!("`{configured}` was not found. {}", vendor.install_hint()))?;
    let Some(cancel) = catalog.logins().begin(vendor) else {
        return Ok(json!({
            "ok": true,
            "state": "already_running",
            "note": format!("A {} sign-in is already in progress.", vendor.product_label()),
        }));
    };
    let mut child = match login_command(&binary, vendor.login_command()).spawn() {
        Ok(child) => child,
        Err(error) => {
            let detail = format!("Could not start `{}`: {error}", binary.display());
            catalog
                .logins()
                .finish(vendor, json!({"ok":false,"detail":detail}));
            bail!(detail);
        }
    };
    let command_line = format!("{} {}", vendor.binary(), vendor.login_command().join(" "));
    let catalog = catalog.clone();
    let config = config.clone();
    tokio::spawn(async move {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<String>(64);
        let mut readers = tokio::task::JoinSet::new();
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        ]
        .into_iter()
        .flatten()
        {
            let sender = sender.clone();
            readers.spawn(async move {
                let mut lines = BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if sender.send(line).await.is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let relay = |line: String| {
            let raw = line.trim();
            if raw.is_empty() {
                return;
            }
            let text = clip(&redact(raw), 2000);
            let payload = json!({"vendor":vendor.id(),"line":text,"url":login_url(raw)});
            catalog.logins().push(vendor, payload.clone());
            catalog.broadcast("account.login", payload);
        };
        let outcome = 'login: loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let _ = child.kill().await;
                    break (false, "Sign-in cancelled".to_owned());
                }
                _ = &mut deadline => {
                    let _ = child.kill().await;
                    break (false, format!("Sign-in timed out after {} seconds", timeout.as_secs()));
                }
                status = child.wait() => {
                    // A browser started by the CLI may keep the pipes open;
                    // allow one bounded tail interval, not a fresh timeout per
                    // line. Stop and the original login deadline still apply.
                    let drain_deadline = tokio::time::sleep(LOGIN_DRAIN_TIMEOUT);
                    tokio::pin!(drain_deadline);
                    loop {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => break 'login (false, "Sign-in cancelled".to_owned()),
                            _ = &mut deadline => break 'login (false, format!("Sign-in timed out after {} seconds", timeout.as_secs())),
                            _ = &mut drain_deadline => break,
                            line = receiver.recv() => match line {
                                Some(line) => relay(line),
                                None => break,
                            },
                        }
                    }
                    break match status {
                        Ok(status) if status.success() => (true, format!("`{command_line}` finished")),
                        Ok(status) => (false, format!("`{command_line}` exited with status {}", status.code().unwrap_or(-1))),
                        Err(error) => (false, format!("`{command_line}` failed: {error}")),
                    };
                }
                Some(line) = receiver.recv() => relay(line),
            }
        };
        // Readers belong to this login, unlike an independently opened browser.
        // Close our pipe ends without signalling that browser or leaving a
        // detached reader waiting indefinitely for its next line.
        readers.shutdown().await;
        drop(receiver);
        // Re-probe: the login state and the account (and thus usage) may
        // have changed.
        catalog.forget_status(vendor).await;
        let status = catalog.refresh(vendor, &config, true).await;
        let done = json!({
            "vendor": vendor.id(),
            "ok": outcome.0,
            "detail": outcome.1,
            "availability": status.availability,
            "availability_label": status.availability.label(),
        });
        catalog.logins().finish(vendor, done.clone());
        catalog.broadcast("account.login.done", done);
    });
    Ok(json!({
        "ok": true,
        "state": "started",
        "note": format!("Running `{}`. Finish signing in with {} in the browser window or with the code it shows.", vendor.login_command().iter().fold(vendor.binary().to_owned(), |a, b| format!("{a} {b}")), vendor.product_label()),
    }))
}

/// Antigravity: start Google's ACP server with ShadowCode's private profile,
/// ask it to `authenticate` with a personal Google account, relay the
/// sign-in link it prints (it also opens the browser), and finish when it
/// answers. The token stays in the private profile; ShadowCode never reads it.
async fn connect_antigravity(
    catalog: &Arc<VendorCatalog>,
    config: &CliAgentsConfig,
    timeout: Duration,
) -> Result<Value> {
    let vendor = Vendor::Antigravity;
    if !config.vendor_enabled(vendor) {
        bail!("Antigravity is disabled in Settings › Advanced");
    }
    let installation = super::antigravity_server::installation(config.binary(vendor))
        .with_context(|| vendor.install_hint().to_owned())?;
    let Some(cancel) = catalog.logins().begin(vendor) else {
        return Ok(json!({
            "ok": true,
            "state": "already_running",
            "note": "An Antigravity sign-in is already in progress.",
        }));
    };
    let mut command = tokio::process::Command::new(&installation.server);
    command
        .args(super::antigravity_server::launch_args())
        .current_dir(super::antigravity_server::home())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("NO_COLOR", "1");
    let run_dir = match super::antigravity_server::prepare(&mut command, &installation, true) {
        Ok(dir) => dir,
        Err(error) => {
            catalog
                .logins()
                .finish(vendor, json!({"ok":false,"detail":format!("{error:#}")}));
            return Err(error);
        }
    };
    #[cfg(target_os = "linux")]
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let detail = format!("Could not start the Antigravity agent: {error}");
            catalog
                .logins()
                .finish(vendor, json!({"ok":false,"detail":detail}));
            bail!(detail);
        }
    };
    let mut stdin = child.stdin.take().context("Antigravity stdin missing")?;
    let handshake = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":1,
            "clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},
            "clientInfo":{"name":"shadowcode","title":"ShadowCode","version":crate::VERSION}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"methodId":super::antigravity_server::AUTH_METHOD}}),
    ];
    for line in handshake {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(format!("{line}\n").as_bytes()).await?;
    }
    let catalog = catalog.clone();
    let config = config.clone();
    tokio::spawn(async move {
        let _run_dir = run_dir;
        let _stdin = stdin;
        let mut stdout = child.stdout.take().map(|s| BufReader::new(s).lines());
        let mut stderr = child.stderr.take().map(|s| BufReader::new(s).lines());
        let relay = |line: &str| {
            let raw = line.trim();
            // The server's own log lines (I0924 …) are noise here.
            if raw.is_empty()
                || raw.len() > 1
                    && raw.as_bytes()[0].is_ascii_uppercase()
                    && raw.as_bytes()[1].is_ascii_digit()
            {
                return;
            }
            let text = clip(&redact(raw), 2000);
            let payload = json!({"vendor":vendor.id(),"line":text,"url":login_url(raw)});
            catalog.logins().push(vendor, payload.clone());
            catalog.broadcast("account.login", payload);
        };
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let outcome = loop {
            tokio::select! {
                Some(Ok(Some(line))) = async { match stderr.as_mut() { Some(s) => Some(s.next_line().await), None => None } } => relay(&line),
                Some(Ok(line)) = async { match stdout.as_mut() { Some(s) => Some(s.next_line().await), None => None } } => {
                    let Some(line) = line else {
                        break (false, "The Antigravity agent stopped before signing in".to_owned());
                    };
                    match serde_json::from_str::<Value>(&line) {
                        Ok(message) if message["id"] == 2 => {
                            break match message.get("error").filter(|e| !e.is_null()) {
                                None => (true, "Signed in to Antigravity".to_owned()),
                                Some(error) => (false, format!(
                                    "Antigravity sign-in failed: {}",
                                    error["data"]["message"].as_str().or_else(|| error["message"].as_str()).unwrap_or("unknown error")
                                )),
                            };
                        }
                        Ok(_) => {}
                        Err(_) => relay(&line),
                    }
                }
                _ = cancel.cancelled() => break (false, "Sign-in cancelled".to_owned()),
                _ = &mut deadline => break (false, format!("Sign-in timed out after {} seconds", timeout.as_secs())),
            }
        };
        let _ = child.kill().await;
        catalog.forget_status(vendor).await;
        let status = catalog.refresh(vendor, &config, true).await;
        let done = json!({
            "vendor": vendor.id(),
            "ok": outcome.0,
            "detail": outcome.1,
            "availability": status.availability,
            "availability_label": status.availability.label(),
        });
        catalog.logins().finish(vendor, done.clone());
        catalog.broadcast("account.login.done", done);
    });
    Ok(json!({
        "ok": true,
        "state": "started",
        "note": "Sign in with your Google account in the browser window that opened (or use the link below). This sign-in belongs to ShadowCode's Antigravity agent.",
    }))
}

/// Run the official logout command after the user confirmed the shared-CLI
/// note, then forget everything cached for the vendor and re-probe.
pub async fn disconnect(
    catalog: &Arc<VendorCatalog>,
    vendor: Vendor,
    config: &CliAgentsConfig,
) -> Result<Value> {
    if vendor == Vendor::Antigravity {
        catalog.logins().cancel(vendor);
        let removed = super::antigravity_server::sign_out()?;
        catalog.forget(vendor).await?;
        let status = catalog.refresh(vendor, config, true).await;
        return Ok(json!({
            "ok": true,
            "ran": [],
            "output": if removed { "Removed ShadowCode's Antigravity profile." } else { "No Antigravity profile was stored." },
            "note": vendor.shared_cli_note(),
            "availability": status.availability,
            "availability_label": status.availability.label(),
        }));
    }
    if vendor.logout_command().is_empty() {
        return Ok(json!({
            "ok": false,
            "ran": [],
            "note": vendor.shared_cli_note(),
        }));
    }
    let configured = config.binary(vendor);
    let binary = resolve_binary(configured)
        .with_context(|| format!("`{configured}` was not found. {}", vendor.install_hint()))?;
    catalog.logins().cancel(vendor);
    let mut command = login_command(&binary, vendor.logout_command());
    let child = command
        .spawn()
        .with_context(|| format!("Could not start `{}`", binary.display()))?;
    // `wait_with_output` owns the child; on timeout it is dropped, and
    // `kill_on_drop` stops it.
    let output = match tokio::time::timeout(LOGOUT_TIMEOUT, child.wait_with_output()).await {
        Ok(output) => output?,
        Err(_) => {
            bail!(
                "`{} {}` did not finish within {} seconds",
                vendor.binary(),
                vendor.logout_command().join(" "),
                LOGOUT_TIMEOUT.as_secs()
            );
        }
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    catalog.forget(vendor).await?;
    let status = catalog.refresh(vendor, config, true).await;
    let ran: Vec<String> = std::iter::once(vendor.binary().to_owned())
        .chain(vendor.logout_command().iter().map(|s| s.to_string()))
        .collect();
    Ok(json!({
        "ok": output.status.success(),
        "ran": ran,
        "output": clip(&redact(text.trim()), 2000),
        "note": vendor.shared_cli_note(),
        "availability": status.availability,
        "availability_label": status.availability.label(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_urls_are_kept_but_callbacks_with_codes_are_not() {
        assert_eq!(
            login_url("Open https://auth.example.com/authorize?client_id=x&code_challenge=abc to continue.").as_deref(),
            Some("https://auth.example.com/authorize?client_id=x&code_challenge=abc")
        );
        assert!(login_url("http://localhost:1455/callback?code=secret").is_none());
        assert!(login_url("https://localhost/cb?code=secret").is_none());
        assert!(login_url("no url here").is_none());
    }
}

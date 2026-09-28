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
use super::{
    catalog::VendorCatalog, clip, probe_lifecycle::PublicationGate, redact, resolve_binary,
    CliAgentsConfig, Vendor,
};
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
use tokio_util::{
    sync::CancellationToken,
    task::{task_tracker::TaskTrackerToken, TaskTracker},
};

pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const LOGOUT_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_LOGIN_LINES: usize = 200;
const LOGIN_DRAIN_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Default)]
struct LoginSession {
    generation: u64,
    started_at: f64,
    lines: Vec<Value>,
    done: Option<Value>,
    cancel: CancellationToken,
    publication: PublicationGate,
}

#[derive(Default)]
struct LoginState {
    sessions: HashMap<Vendor, LoginSession>,
    closing: bool,
    next_generation: u64,
}

/// One admitted login per vendor. Tracking starts atomically with admission,
/// before a child is spawned, and ends only after its worker future returns.
#[derive(Default)]
pub struct Logins {
    state: Mutex<LoginState>,
    workers: TaskTracker,
}

struct LoginOperation {
    catalog: Arc<VendorCatalog>,
    vendor: Vendor,
    generation: u64,
    cancel: CancellationToken,
    publication: PublicationGate,
    deadline: tokio::time::Instant,
    timeout: Duration,
    tracking: Option<TaskTrackerToken>,
    finished: bool,
}

impl LoginOperation {
    fn finish(&mut self, mut done: Value) {
        let published = if let Ok(mut state) = self.catalog.logins().state.lock() {
            match state.sessions.get_mut(&self.vendor) {
                Some(session)
                    if session.generation == self.generation && session.done.is_none() =>
                {
                    // Cancel and terminal publication use the same mutex. A
                    // cancel acknowledged before publication must win.
                    if session.cancel.is_cancelled() {
                        done["ok"] = json!(false);
                        done["detail"] = json!("Sign-in cancelled");
                    } else if tokio::time::Instant::now() >= self.deadline {
                        done["ok"] = json!(false);
                        done["detail"] = json!(format!(
                            "Sign-in timed out after {} seconds",
                            self.timeout.as_secs()
                        ));
                    }
                    session.done = Some(done.clone());
                    true
                }
                _ => false,
            }
        } else {
            false
        };
        self.finished = true;
        if published {
            self.catalog.broadcast("account.login.done", done);
        }
    }
    fn fail(&mut self, detail: &str) {
        self.finish(json!({"vendor":self.vendor.id(),"ok":false,"detail":detail,
            "availability":"unavailable","availability_label":"Unavailable"}));
    }
}
impl Drop for LoginOperation {
    fn drop(&mut self) {
        if !self.finished {
            self.fail("Sign-in interrupted before completion");
        }
    }
}

impl Logins {
    fn begin(
        &self,
        vendor: Vendor,
        timeout: Duration,
        catalog: &Arc<VendorCatalog>,
    ) -> Result<Option<LoginOperation>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Login registry lock poisoned"))?;
        if state.closing {
            bail!("Login supervisor is shutting down");
        }
        if state
            .sessions
            .get(&vendor)
            .is_some_and(|s| s.done.is_none())
        {
            return Ok(None);
        }
        let cancel = CancellationToken::new();
        let publication = PublicationGate::default();
        state.next_generation += 1;
        let generation = state.next_generation;
        // TaskTracker::close alone does not forbid new tasks. Both the closing
        // flag and this reservation are therefore protected by this mutex.
        let tracking = self.workers.token();
        let deadline = tokio::time::Instant::now() + timeout;
        state.sessions.insert(
            vendor,
            LoginSession {
                generation,
                started_at: crate::now(),
                cancel: cancel.clone(),
                publication: publication.clone(),
                ..Default::default()
            },
        );
        Ok(Some(LoginOperation {
            catalog: catalog.clone(),
            vendor,
            generation,
            cancel,
            publication,
            deadline,
            timeout,
            tracking: Some(tracking),
            finished: false,
        }))
    }
    fn push(&self, vendor: Vendor, generation: u64, line: Value) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(session) = state
                .sessions
                .get_mut(&vendor)
                .filter(|s| s.generation == generation && s.done.is_none())
            {
                if session.lines.len() >= MAX_LOGIN_LINES {
                    session.lines.remove(0);
                }
                session.lines.push(line);
            }
        }
    }
    pub fn status(&self, vendor: Vendor) -> Value {
        let Ok(state) = self.state.lock() else {
            return json!({"running":false,"cancellation_requested":false});
        };
        match state.sessions.get(&vendor) {
            Some(session) => json!({"vendor":vendor.id(),"running":session.done.is_none(),
                "cancellation_requested":session.done.is_none() && session.cancel.is_cancelled(),
                "started_at":session.started_at,"lines":session.lines,"done":session.done}),
            None => {
                json!({"vendor":vendor.id(),"running":false,"cancellation_requested":false,"lines":[],"done":null})
            }
        }
    }
    pub fn running(&self, vendor: Vendor) -> bool {
        self.state
            .lock()
            .map(|s| s.sessions.get(&vendor).is_some_and(|s| s.done.is_none()))
            .unwrap_or(false)
    }
    pub fn cancel(&self, vendor: Vendor) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        match state.sessions.get(&vendor) {
            Some(session) if session.done.is_none() => {
                session.publication.cancel(&session.cancel);
                true
            }
            _ => false,
        }
    }
    pub fn cancel_all(&self) {
        if let Ok(state) = self.state.lock() {
            for session in state.sessions.values() {
                session.publication.cancel(&session.cancel);
            }
        }
    }
    pub fn begin_shutdown(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Login registry lock poisoned"))?;
        state.closing = true;
        for session in state.sessions.values() {
            session.publication.cancel(&session.cancel);
        }
        self.workers.close();
        Ok(())
    }
    /// Cancel-safe: a timed-out engine shutdown leaves every admitted worker
    /// tracked. A later wait observes the same outstanding ownership.
    pub async fn wait_shutdown(&self) {
        self.workers.wait().await;
    }
}
impl Drop for Logins {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

/// The final account observation belongs to the original login operation, not
/// a new uncancellable operation with a fresh discovery deadline.
async fn complete_login(
    mut operation: LoginOperation,
    outcome: (bool, String),
    config: &CliAgentsConfig,
) {
    let catalog = operation.catalog.clone();
    let vendor = operation.vendor;
    catalog.forget_status(vendor).await;
    let status = if outcome.0 {
        // The scoped discovery helpers observe this original control and
        // explicitly kill + wait for owned children. Keep awaiting cleanup:
        // selecting/dropping refresh here would discard reaping ownership.
        Some(
            catalog
                .refresh_for_login(
                    vendor,
                    config,
                    operation.cancel.clone(),
                    operation.deadline,
                    operation.publication.clone(),
                )
                .await,
        )
    } else {
        None
    };
    let availability = status
        .as_ref()
        .map(|s| s.availability)
        .unwrap_or(super::picker::Availability::Unavailable);
    operation.finish(
        json!({"vendor":vendor.id(),"ok":outcome.0,"detail":outcome.1,
        "availability":availability,"availability_label":availability.label()}),
    );
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

/// Stop a login child: SIGTERM first, so a wrapper (the npm `codex` script
/// runs the real program as its child and passes SIGTERM on) can stop its
/// own login server, then SIGKILL after a short grace period. SIGKILL alone
/// cannot be passed on, and left that login server listening.
async fn stop_login(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        // SAFETY: the child is not reaped yet, so its PID is still its own.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        if tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .is_ok()
        {
            return;
        }
    }
    let _ = child.kill().await;
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
    let Some(mut operation) = catalog.logins().begin(vendor, timeout, catalog)? else {
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
            operation.fail(&detail);
            bail!(detail);
        }
    };
    let command_line = format!("{} {}", vendor.binary(), vendor.login_command().join(" "));
    let catalog = catalog.clone();
    let config = config.clone();
    let tracking = operation
        .tracking
        .take()
        .expect("admitted login owns tracking");
    tokio::spawn(async move {
        let _tracking = tracking;
        async move {
            let cancel = operation.cancel.clone();
            let generation = operation.generation;
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
            let deadline = tokio::time::sleep_until(operation.deadline);
            tokio::pin!(deadline);
            let relay = |line: String| {
                let raw = line.trim();
                if raw.is_empty() {
                    return;
                }
                let text = clip(&redact(raw), 2000);
                let payload = json!({"vendor":vendor.id(),"line":text,"url":login_url(raw)});
                catalog.logins().push(vendor, generation, payload.clone());
                catalog.broadcast("account.login", payload);
            };
            let outcome = 'login: loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        stop_login(&mut child).await;
                        break (false, "Sign-in cancelled".to_owned());
                    }
                    _ = &mut deadline => {
                        stop_login(&mut child).await;
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
            complete_login(operation, outcome, &config).await;
        }.await;
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
    let Some(mut operation) = catalog.logins().begin(vendor, timeout, catalog)? else {
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
            operation.fail(&format!("{error:#}"));
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
            operation.fail(&detail);
            bail!(detail);
        }
    };
    let catalog = catalog.clone();
    let config = config.clone();
    let tracking = operation
        .tracking
        .take()
        .expect("admitted login owns tracking");
    tokio::spawn(async move {
        let _tracking = tracking;
        async move {
            let cancel = operation.cancel.clone();
            let generation = operation.generation;
            let _run_dir = run_dir;
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
                catalog.logins().push(vendor, generation, payload.clone());
                catalog.broadcast("account.login", payload);
            };
            let deadline = tokio::time::sleep_until(operation.deadline);
            tokio::pin!(deadline);
            let exchange = async {
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
                Ok::<_, anyhow::Error>(loop {
                    tokio::select! {
                        result = async { stderr.as_mut().expect("guarded stderr").next_line().await }, if stderr.is_some() => {
                            match result {
                                Ok(Some(line)) => relay(&line),
                                Ok(None) => stderr = None,
                                Err(error) => break (false, format!("Could not read Antigravity sign-in diagnostics: {error}")),
                            }
                        }
                        result = async { match stdout.as_mut() { Some(s) => s.next_line().await, None => Ok(None) } } => {
                            let line = match result {
                                Ok(Some(line)) => line,
                                Ok(None) => break (false, "The Antigravity agent stopped before signing in".to_owned()),
                                Err(error) => break (false, format!("Could not read Antigravity sign-in response: {error}")),
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
                    }
                })
            };
            let outcome = tokio::select! {
                biased;
                _ = cancel.cancelled() => (false, "Sign-in cancelled".to_owned()),
                _ = &mut deadline => (false, format!("Sign-in timed out after {} seconds", timeout.as_secs())),
                reply = exchange => match reply {
                    Ok(outcome) => outcome,
                    Err(error) => (false, format!("Antigravity sign-in failed: {error:#}")),
                },
            };
            let _ = child.kill().await;
            complete_login(operation, outcome, &config).await;
        }.await;
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
    #[tokio::test]
    async fn login_status_preserves_vendor_scoped_cancellation_until_terminal() {
        let catalog = Arc::new(VendorCatalog::new());
        assert_eq!(
            catalog.logins().status(Vendor::Codex)["cancellation_requested"],
            false
        );
        let mut first = catalog
            .logins()
            .begin(Vendor::Codex, Duration::from_secs(5), &catalog)
            .unwrap()
            .unwrap();
        let mut other = catalog
            .logins()
            .begin(Vendor::Claude, Duration::from_secs(5), &catalog)
            .unwrap()
            .unwrap();
        assert_eq!(
            catalog.logins().status(Vendor::Codex)["cancellation_requested"],
            false
        );
        assert!(catalog.logins().cancel(Vendor::Codex));
        let stopping = catalog.logins().status(Vendor::Codex);
        assert_eq!(stopping["running"], true);
        assert_eq!(stopping["cancellation_requested"], true);
        assert_eq!(
            catalog.logins().status(Vendor::Claude)["cancellation_requested"],
            false
        );
        first.finish(json!({"ok":true,"detail":"process reaped"}));
        let stopped = catalog.logins().status(Vendor::Codex);
        assert_eq!(stopped["running"], false);
        assert_eq!(stopped["cancellation_requested"], false);
        assert_eq!(stopped["done"]["ok"], false);
        let mut retry = catalog
            .logins()
            .begin(Vendor::Codex, Duration::from_secs(5), &catalog)
            .unwrap()
            .unwrap();
        assert_eq!(
            catalog.logins().status(Vendor::Codex)["cancellation_requested"],
            false
        );
        retry.fail("fixture finished");
        other.fail("fixture finished");
    }

    #[tokio::test]
    async fn accepted_cancel_wins_publication_and_old_generation_cannot_write_retry() {
        let catalog = Arc::new(VendorCatalog::new());
        let mut first = catalog
            .logins()
            .begin(Vendor::Codex, Duration::from_secs(5), &catalog)
            .unwrap()
            .unwrap();
        let old_generation = first.generation;
        assert!(catalog.logins().cancel(Vendor::Codex));
        first.finish(json!({"ok":true,"detail":"old CLI exited zero"}));
        assert_eq!(catalog.logins().status(Vendor::Codex)["done"]["ok"], false);
        let mut second = catalog
            .logins()
            .begin(Vendor::Codex, Duration::from_secs(5), &catalog)
            .unwrap()
            .unwrap();
        catalog
            .logins()
            .push(Vendor::Codex, old_generation, json!({"line":"stale"}));
        first.finish(json!({"ok":true,"detail":"stale final result"}));
        let current = catalog.logins().status(Vendor::Codex);
        assert_eq!(current["running"], true);
        assert!(current["done"].is_null());
        assert_eq!(current["lines"], json!([]));
        second.fail("fixture finished");
    }

    #[tokio::test]
    async fn shutdown_wait_retains_admitted_ownership_after_waiter_timeout() {
        let catalog = Arc::new(VendorCatalog::new());
        let mut operation = catalog
            .logins()
            .begin(Vendor::Codex, Duration::from_secs(5), &catalog)
            .unwrap()
            .unwrap();
        catalog.logins().begin_shutdown().unwrap();
        assert!(catalog
            .logins()
            .begin(Vendor::Claude, Duration::from_secs(5), &catalog)
            .is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(5), catalog.logins().wait_shutdown())
                .await
                .is_err()
        );
        operation.fail("fixture finished");
        // A terminal session is not a release of worker ownership.
        assert!(
            tokio::time::timeout(Duration::from_millis(5), catalog.logins().wait_shutdown())
                .await
                .is_err()
        );
        drop(operation);
        tokio::time::timeout(Duration::from_millis(100), catalog.logins().wait_shutdown())
            .await
            .unwrap();
    }
}

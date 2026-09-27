//! Spawn a vendor CLI in the trusted workspace and translate its stream.
//!
//! The child inherits the user's login environment so the official CLI can
//! read its own credentials. ShadowCode never opens those files, never injects
//! its tools or bubblewrap, and kills the process group on cancel.
use super::{
    adapter_for, clip,
    lines::{BoundedLines, Line, MAX_DIAGNOSTIC_BYTES},
    redact, resolve_binary, ApprovalPrompt, CliAdapter, CliAgentsConfig, LaunchOptions, Update,
    Vendor, VendorAnswer, MAX_LINE_BYTES, MAX_MALFORMED_LINES,
};
use crate::{
    approvals::{Answer, Approval, ApprovalHub, Grant},
    events::TaskEvents,
    models::Usage,
    steering::SteerControl,
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, ChildStdin, Command},
};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};

const TEXT_FLUSH_INTERVAL: Duration = Duration::from_millis(50);
const INPUT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const INTERRUPT_WRITE_TIMEOUT: Duration = Duration::from_millis(250);

struct ProcessGroup(u32);
impl ProcessGroup {
    #[cfg(target_os = "linux")]
    fn descendants(pid: u32) -> Vec<u32> {
        let path = format!("/proc/{pid}/task/{pid}/children");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .split_whitespace()
            .filter_map(|value| value.parse().ok())
            .flat_map(|child| {
                let mut all = vec![child];
                all.extend(Self::descendants(child));
                all
            })
            .collect()
    }
    fn terminate(&self) {
        if self.0 == 0 {
            return;
        }
        #[cfg(target_os = "linux")]
        let descendants = Self::descendants(self.0);
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGTERM);
            libc::kill(self.0 as i32, libc::SIGTERM);
            #[cfg(target_os = "linux")]
            for child in descendants {
                libc::kill(child as i32, libc::SIGTERM);
            }
        }
    }
    fn kill(&mut self) {
        if self.0 == 0 {
            return;
        }
        #[cfg(target_os = "linux")]
        let descendants = Self::descendants(self.0);
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
            libc::kill(self.0 as i32, libc::SIGKILL);
            #[cfg(target_os = "linux")]
            for child in descendants {
                libc::kill(child as i32, libc::SIGKILL);
            }
        }
        self.0 = 0;
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Inputs for one vendor-agent task.
pub struct Request<'a> {
    pub vendor: Vendor,
    pub options: LaunchOptions,
    pub config: &'a CliAgentsConfig,
    pub prompt: String,
    pub images: Vec<super::PromptImage>,
    pub session_id: String,
    pub task_id: String,
    pub job_id: String,
    pub events: &'a TaskEvents,
    pub approvals: &'a ApprovalHub,
    pub cancel: CancellationToken,
    pub steer: &'a SteerControl,
    /// The user's permission mode asks before shell commands / edits. The
    /// approval-less `codex exec` fallback is refused while this is set.
    pub approvals_required: bool,
    /// Receives plan usage pushed during the turn (Codex rate-limit updates).
    pub catalog: Option<std::sync::Arc<super::catalog::VendorCatalog>>,
}

/// What a finished vendor run produced.
#[derive(Clone, Debug, Default)]
pub struct RunOutcome {
    pub text: String,
    /// Per-turn token counts the vendor reported (never plan usage).
    pub usage: Usage,
    /// False when the protocol reported no token counts at all.
    pub usage_reported: bool,
    /// Vendor session id to resume on the next turn of this conversation.
    pub native_session: Option<String>,
}

/// The vendor account hit its plan limit. The job stops with status
/// `limit_reached`; ShadowCode never retries, buys credits, or redeems a
/// reset on the user's behalf.
#[derive(Debug, Clone)]
pub struct LimitReached {
    pub vendor: Vendor,
    pub detail: String,
    pub usage: super::usage::UsageSnapshot,
}
impl std::fmt::Display for LimitReached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} plan limit reached: {}. ShadowCode never retries on the same plan or buys more usage.",
            self.vendor.product_label(),
            self.detail
        )
    }
}
impl std::error::Error for LimitReached {}

/// Run the vendor CLI until the turn finishes, fails, or is cancelled.
///
/// Codex only: when `codex app-server` is missing, or fails before a turn
/// was sent (spawn failure, rejected `initialize`, exit during the
/// handshake), the one-shot `codex exec --json` path may run instead. It has
/// no approval channel, so it is refused while the user's permission mode
/// asks before actions. Nothing is ever re-run once a turn started.
pub async fn run(request: Request<'_>) -> Result<RunOutcome> {
    ensure_ready(request.vendor, &request)?;
    if request.vendor == Vendor::Codex && !codex_app_server_available(&request.options.binary).await
    {
        return run_exec_fallback(&request, "this Codex CLI has no `app-server` command").await;
    }
    let mut reached_ready = false;
    match run_once(request.vendor, false, &request, &mut reached_ready).await {
        Err(error)
            if request.vendor == Vendor::Codex
                && !reached_ready
                && request.images.is_empty()
                && error.downcast_ref::<LimitReached>().is_none()
                && !request.cancel.is_cancelled() =>
        {
            run_exec_fallback(&request, &format!("{error:#}")).await
        }
        other => other,
    }
}

async fn run_exec_fallback(request: &Request<'_>, reason: &str) -> Result<RunOutcome> {
    let reason = clip(&redact(reason), 400);
    if request.approvals_required {
        bail!(
            "Codex app-server could not start a session ({reason}). The `codex exec` fallback has no approval channel, so ShadowCode will not run it while shell commands require approval. Update the Codex CLI so its app-server starts."
        );
    }
    request.events.emit(
        "agent.warning",
        json!({"text":format!("Codex app-server could not start a session ({reason}); using `codex exec`, which runs the whole turn without approval prompts.")}),
    )?;
    let mut reached_ready = false;
    run_once(Vendor::Codex, true, request, &mut reached_ready).await
}

/// `codex app-server` is used when help mentions it; otherwise exec fallback.
pub async fn codex_app_server_available(binary: &str) -> bool {
    let help = short_output(binary, &["--help"]).await.unwrap_or_default();
    if help.contains("app-server") {
        return true;
    }
    match short_output(binary, &["app-server", "--help"]).await {
        Some(text) => {
            let lower = text.to_ascii_lowercase();
            !["unknown", "unrecognized", "no such", "not found"]
                .iter()
                .any(|needle| lower.contains(needle))
        }
        None => false,
    }
}

async fn short_output(binary: &str, args: &[&str]) -> Option<String> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .ok()?
        .ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some(text)
}

async fn run_once(
    vendor: Vendor,
    codex_exec_fallback: bool,
    request: &Request<'_>,
    reached_ready: &mut bool,
) -> Result<RunOutcome> {
    ensure_ready(vendor, request)?;
    let mut adapter = adapter_for(vendor, codex_exec_fallback);
    let (program, args) = adapter.command(&request.options);
    let env = adapter.env(&request.options);
    let (mut child, _run_dir) =
        spawn_vendor(vendor, &program, &args, &env, &request.options.workspace)?;
    let pid = child.id().context("Vendor CLI has no process ID")?;
    let mut group = ProcessGroup(pid);
    let mut stdin = Some(child.stdin.take().context("Vendor CLI stdin missing")?);
    let stdout = child.stdout.take().context("Vendor CLI stdout missing")?;
    let stderr = child.stderr.take().context("Vendor CLI stderr missing")?;
    let mut reader = BoundedLines::new(stdout, MAX_LINE_BYTES);
    let sign_in_needed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _stderr_drain = AbortOnDropHandle::new(tokio::spawn(drain_stderr(
        stderr,
        request.events.clone(),
        sign_in_needed.clone(),
    )));
    let mut outgoing = adapter.on_start(&request.options);
    outgoing.extend(adapter.prompt(&request.prompt, &request.images)?);
    send_lines(request, &mut stdin, &outgoing).await?;
    if adapter.one_shot() {
        // One-shot CLIs (`codex exec -`) read the prompt until EOF.
        stdin = None;
    }
    let mut collected = String::new();
    let mut usage = Usage::default();
    let mut usage_reported = false;
    let mut native_session: Option<String> = None;
    let mut malformed = 0usize;
    let mut last_line = Instant::now();
    let stall = Duration::from_secs(request.config.stall_timeout_sec.max(30));
    let mut message_id = crate::id();
    let mut pending_text = String::new();
    let mut last_flush = Instant::now();
    let mut interrupted_turn = false;
    let mut finished = false;
    let mut final_text = None;
    loop {
        if request.cancel.is_cancelled() {
            interrupt_vendor(&mut stdin, &mut adapter).await;
            group.terminate();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            group.kill();
            flush_text(request, &message_id, &mut pending_text)?;
            bail!("Task cancelled. The vendor CLI was stopped; its file changes remain on disk.");
        }
        if sign_in_needed.load(std::sync::atomic::Ordering::Acquire) {
            group.terminate();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            group.kill();
            bail!("{}", super::acp::ANTIGRAVITY_SIGN_IN);
        }
        if request.steer.is_paused() && !interrupted_turn && !finished {
            send_lines(request, &mut stdin, &adapter.interrupt()).await?;
            interrupted_turn = true;
        }
        // Text must reach the transcript during generation, including a short
        // delta followed by a quiet provider. Check between frames as well as
        // on idle polls so a continuous stream cannot starve the flush.
        if last_flush.elapsed() >= TEXT_FLUSH_INTERVAL {
            flush_text(request, &message_id, &mut pending_text)?;
            last_flush = Instant::now();
        }
        let poll_interval = if pending_text.is_empty() {
            Duration::from_millis(250)
        } else {
            TEXT_FLUSH_INTERVAL.saturating_sub(last_flush.elapsed())
        };
        let remaining = stall.saturating_sub(last_line.elapsed());
        let read = tokio::time::timeout(remaining.min(poll_interval), reader.next_protocol_line());
        match read.await {
            Ok(Ok(None)) => {
                flush_text(request, &message_id, &mut pending_text)?;
                let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
                    .await
                    .ok()
                    .and_then(Result::ok);
                group.kill();
                if finished {
                    break;
                }
                let code = status.and_then(|s| s.code()).unwrap_or(-1);
                bail!(
                    "{} exited before finishing the turn (status {code})",
                    vendor.label()
                );
            }
            Ok(Ok(Some(line))) => {
                last_line = Instant::now();
                let step = adapter.on_line(&line)?;
                // Once a prompt may be written, input failure must not start
                // an exec fallback and risk submitting the task twice.
                *reached_ready |= adapter.ready();
                send_lines(request, &mut stdin, &step.send).await?;
                let mut saw_protocol = false;
                for update in step.updates {
                    match update {
                        Update::Warning(text) => {
                            if text.contains("Ignored a non-JSON")
                                || text.contains("Ignored a non-object")
                                || text.contains("without method or id")
                                || text.contains("without type")
                            {
                                malformed += 1;
                                ensure!(
                                    malformed <= MAX_MALFORMED_LINES,
                                    "{} sent too many malformed lines",
                                    vendor.label()
                                );
                            } else {
                                malformed = 0;
                            }
                            request.events.emit("agent.warning", json!({"text":text}))?;
                        }
                        other => {
                            malformed = 0;
                            saw_protocol = true;
                            apply_update(
                                request,
                                vendor,
                                other,
                                &mut collected,
                                &mut usage,
                                &mut usage_reported,
                                &mut native_session,
                                &mut message_id,
                                &mut pending_text,
                                &mut finished,
                                &mut final_text,
                                &mut stdin,
                                &mut adapter,
                            )
                            .await?;
                        }
                    }
                }
                if saw_protocol {
                    malformed = 0;
                }
                if finished {
                    if adapter.one_shot() {
                        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                    }
                    group.kill();
                    break;
                }
            }
            Ok(Err(error)) => {
                group.kill();
                return Err(error);
            }
            Err(_) if last_line.elapsed() >= stall => {
                group.terminate();
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                group.kill();
                bail!(
                    "{} produced no output for {} seconds",
                    vendor.label(),
                    request.config.stall_timeout_sec
                );
            }
            Err(_) => {
                if request.steer.is_paused() && finished {
                    break;
                }
            }
        }
        if finished && request.steer.is_paused() {
            flush_text(request, &message_id, &mut pending_text)?;
            if let Some(follow_up) = wait_for_steer(request).await? {
                finished = false;
                interrupted_turn = false;
                message_id = crate::id();
                outgoing = adapter.prompt(&follow_up, &[])?;
                send_lines(request, &mut stdin, &outgoing).await?;
            } else {
                group.kill();
                break;
            }
        }
    }
    flush_text(request, &message_id, &mut pending_text)?;
    if let Some(text) = final_text {
        if collected.is_empty() {
            collected = text;
        }
    }
    if native_session.is_none() {
        native_session = adapter.native_session();
    }
    Ok(RunOutcome {
        text: collected,
        usage,
        usage_reported,
        native_session,
    })
}

fn ensure_ready(vendor: Vendor, request: &Request<'_>) -> Result<()> {
    if !request.config.vendor_enabled(vendor) {
        bail!(
            "{} is disabled in Settings → Advanced (cli_agents)",
            vendor.label()
        );
    }
    if resolve_binary(&request.options.binary).is_none()
        && !Path::new(&request.options.binary).is_file()
    {
        bail!(
            "{} was not found on PATH. {}",
            vendor.binary(),
            vendor.install_hint()
        );
    }
    Ok(())
}

fn spawn_vendor(
    vendor: Vendor,
    program: &str,
    args: &[String],
    env: &[(String, String)],
    workspace: &Path,
) -> Result<(Child, Option<super::antigravity_server::RunDir>)> {
    ensure_workspace(workspace)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("NO_COLOR", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("PAGER", "cat");
    // Inherit the user environment so the official CLI can use its own login,
    // minus provider API keys: a subscription row must never be billed per
    // token. No bubblewrap, no ShadowCode tools or secrets.
    super::scrub_api_keys(&mut command);
    // Antigravity's server gets ShadowCode's private profile, its own temp
    // directory and no way to open a browser.
    let run_dir = if vendor == Vendor::Antigravity {
        let installation = super::antigravity_server::Installation {
            server: Path::new(program).to_owned(),
            harness: Path::new(program).with_file_name(super::antigravity_server::HARNESS_FILE),
        };
        Some(super::antigravity_server::prepare(
            &mut command,
            &installation,
            false,
        )?)
    } else {
        None
    };
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(target_os = "linux")]
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command
        .spawn()
        .with_context(|| format!("Could not start {program}"))?;
    Ok((child, run_dir))
}

fn ensure_workspace(workspace: &Path) -> Result<()> {
    anyhow::ensure!(workspace.is_dir(), "Vendor CLI workspace does not exist");
    Ok(())
}

async fn send_lines(
    request: &Request<'_>,
    stdin: &mut Option<ChildStdin>,
    lines: &[String],
) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let Some(stdin) = stdin.as_mut() else {
        bail!("The vendor CLI input is closed");
    };
    send_lines_bounded(stdin, lines, &request.cancel, INPUT_WRITE_TIMEOUT).await
}

async fn send_lines_bounded<W: tokio::io::AsyncWrite + Unpin>(
    stdin: &mut W,
    lines: &[String],
    cancel: &CancellationToken,
    deadline: Duration,
) -> Result<()> {
    // A cancelled/timed-out write may have sent a partial frame. Return an
    // error and stop the owning runtime; never retry that frame on this pipe.
    tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("Task cancelled while sending input to the vendor CLI"),
        result = tokio::time::timeout(deadline, write_lines(stdin, lines)) => {
            result.map_err(|_| anyhow::anyhow!(
                "Vendor CLI did not accept input within {} seconds", deadline.as_secs_f64()
            ))?
        }
    }
}

async fn write_lines<W: tokio::io::AsyncWrite + Unpin>(
    stdin: &mut W,
    lines: &[String],
) -> Result<()> {
    for line in lines {
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    }
    if !lines.is_empty() {
        stdin.flush().await?;
    }
    Ok(())
}

async fn interrupt_vendor(stdin: &mut Option<ChildStdin>, adapter: &mut Box<dyn CliAdapter>) {
    let lines = adapter.interrupt();
    if let Some(stdin) = stdin.as_mut() {
        // Best effort before mandatory process termination. This path also
        // runs after cancellation, so it cannot use the cancelled task token.
        let _ = tokio::time::timeout(INTERRUPT_WRITE_TIMEOUT, write_lines(stdin, &lines)).await;
    }
}

async fn drain_stderr<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    stderr: R,
    events: TaskEvents,
    sign_in_needed: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let mut reader = BoundedLines::new(stderr, MAX_DIAGNOSTIC_BYTES);
    while let Ok(Some(line)) = reader.next_line().await {
        let Line::Text(line) = line else {
            let _ = events.emit(
                "agent.warning",
                json!({"text":"vendor stderr: omitted oversized diagnostic line"}),
            );
            continue;
        };
        let line = redact(line.trim());
        if line.is_empty() {
            continue;
        }
        // Antigravity's server prints a Google sign-in link when its
        // sign-in is missing or expired; the run cannot continue.
        if super::antigravity_server::is_sign_in_prompt(&line) {
            sign_in_needed.store(true, std::sync::atomic::Ordering::Release);
            let _ = events.emit(
                "agent.warning",
                json!({"text": super::acp::ANTIGRAVITY_SIGN_IN}),
            );
            continue;
        }
        let _ = events.emit(
            "agent.warning",
            json!({"text":format!("vendor stderr: {}", clip(&line, 400))}),
        );
    }
}

fn flush_text(request: &Request<'_>, message_id: &str, pending: &mut String) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    request.events.emit(
        "model.stream",
        json!({"text":pending.clone(),"message_id":message_id}),
    )?;
    pending.clear();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn apply_update(
    request: &Request<'_>,
    vendor: Vendor,
    update: Update,
    collected: &mut String,
    usage: &mut Usage,
    usage_reported: &mut bool,
    native_session: &mut Option<String>,
    message_id: &mut String,
    pending_text: &mut String,
    finished: &mut bool,
    final_text: &mut Option<String>,
    stdin: &mut Option<ChildStdin>,
    adapter: &mut Box<dyn CliAdapter>,
) -> Result<()> {
    match update {
        Update::Text(text) => {
            collected.push_str(&text);
            pending_text.push_str(&text);
            if pending_text.len() >= 4000 {
                flush_text(request, message_id, pending_text)?;
            }
        }
        Update::ToolStarted { id, name, detail } => {
            flush_text(request, message_id, pending_text)?;
            request.events.emit(
                "tool.started",
                json!({"tool":name,"call_id":id,"arguments":detail}),
            )?;
        }
        Update::ToolCompleted {
            id,
            name,
            success,
            output,
        } => {
            request.events.emit(
                "tool.completed",
                json!({
                    "tool":name,
                    "call_id":id,
                    "success":success,
                    "output":output,
                    "output_preview":crate::tools::truncate(&output.to_string(), 2000),
                    "error":""
                }),
            )?;
        }
        Update::FilesChanged { paths, detail } => {
            request.events.emit(
                "files.changed",
                json!({"paths":paths,"detail":detail,"vendor":vendor.id()}),
            )?;
        }
        Update::Approval(prompt) => {
            flush_text(request, message_id, pending_text)?;
            if request.options.read_only
                && matches!(
                    prompt.kind.as_str(),
                    "command" | "file_change" | "permissions"
                )
            {
                // Plan/Review tasks are read-only in ShadowCode even when the
                // vendor asks: deny without prompting and say so.
                request.events.emit(
                    "agent.warning",
                    json!({"text":format!(
                        "Denied automatically: this task is read-only, so {}'s request was declined ({}).",
                        vendor.product_label(),
                        clip(&prompt.command, 300)
                    ),"vendor":vendor.id(),"kind":prompt.kind}),
                )?;
                send_lines(request, stdin, &adapter.approve(&prompt.request_id, false)?).await?;
                return Ok(());
            }
            let answer = request_approval(request, prompt.clone(), adapter.deny_note()).await?;
            let reply = VendorAnswer {
                allow: answer.allow,
                // The first "Allow for this task" maps to the vendor's own
                // session-wide allow where the protocol has one; requests
                // ShadowCode already allowed are answered once.
                for_session: answer.for_task && !answer.automatic,
                note: answer.note.clone(),
            };
            send_lines(request, stdin, &adapter.answer(&prompt.request_id, &reply)?).await?;
        }
        Update::Warning(text) => {
            request.events.emit("agent.warning", json!({"text":text}))?;
        }
        Update::Usage {
            input,
            output: completion,
            cached,
        } => {
            *usage_reported = true;
            usage.prompt_tokens = usage.prompt_tokens.saturating_add(input);
            usage.completion_tokens = usage.completion_tokens.saturating_add(completion);
            usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
            usage.cached_tokens = usage.cached_tokens.saturating_add(cached);
            usage.turns = usage.turns.saturating_add(1);
            usage.source = "vendor".into();
        }
        Update::VendorCost { total_usd } => {
            usage.cost_usd = Some(total_usd);
            usage.source = "vendor".into();
        }
        Update::TurnCompleted { text, interrupted } => {
            flush_text(request, message_id, pending_text)?;
            // The streamed reply is complete, so the final result can
            // recognise it instead of repeating it.
            request.events.emit(
                "model.stream_end",
                json!({"message_id": message_id, "complete": !interrupted}),
            )?;
            if let Some(text) = text {
                if !text.is_empty() {
                    collected.push_str(&text);
                    *final_text = Some(text);
                }
            }
            if interrupted && request.steer.is_paused() {
                if let Some(follow_up) = wait_for_steer(request).await? {
                    *message_id = crate::id();
                    send_lines(request, stdin, &adapter.prompt(&follow_up, &[])?).await?;
                    return Ok(());
                }
            }
            *finished = true;
        }
        Update::TurnFailed(error) => {
            flush_text(request, message_id, pending_text)?;
            request.events.emit(
                "model.stream_end",
                json!({"message_id": message_id, "complete": false}),
            )?;
            if super::is_limit_error(&error) {
                return Err(limit_reached(request, vendor, error, stdin, adapter).await);
            }
            bail!("{error}");
        }
        Update::LimitReached(detail) => {
            flush_text(request, message_id, pending_text)?;
            return Err(limit_reached(request, vendor, detail, stdin, adapter).await);
        }
        Update::RateLimits(snapshot) => {
            let usage = match &request.catalog {
                Some(catalog) => {
                    catalog
                        .apply_rate_limits(vendor, &snapshot, &request.options.model)
                        .await
                }
                None => super::usage::UsageSnapshot::from_codex(
                    &json!({"rateLimits": snapshot}),
                    Some(&request.options.model),
                    crate::now(),
                ),
            };
            request
                .events
                .emit("usage.updated", json!({"vendor":vendor.id(),"usage":usage}))?;
        }
        Update::NativeSession { id } => {
            if native_session.as_deref() != Some(id.as_str()) {
                *native_session = Some(id.clone());
                request.events.emit(
                    "vendor.session",
                    json!({"vendor":vendor.id(),"session_id":id,"job_id":request.job_id}),
                )?;
            }
        }
    }
    Ok(())
}

/// Stop the turn at a plan limit: interrupt the vendor, record
/// `limit.reached`, and return the typed error that ends the job.
async fn limit_reached(
    request: &Request<'_>,
    vendor: Vendor,
    detail: String,
    stdin: &mut Option<ChildStdin>,
    adapter: &mut Box<dyn CliAdapter>,
) -> anyhow::Error {
    interrupt_vendor(stdin, adapter).await;
    let detail = clip(&redact(&detail), 600);
    let usage = match &request.catalog {
        Some(catalog) => {
            catalog
                .limit_usage(vendor, &request.options.model, &detail)
                .await
        }
        None => super::usage::UsageSnapshot::limit_reached(&vendor.provider(), &detail),
    };
    if let Err(error) = request.events.emit(
        "limit.reached",
        json!({"vendor":vendor.id(),"usage":usage,"detail":detail,"job_id":request.job_id}),
    ) {
        return error;
    }
    anyhow::Error::new(LimitReached {
        vendor,
        detail,
        usage,
    })
}

/// What "Allow for this task" covers for a vendor prompt: commands with the
/// same program and subcommand, the vendor's file changes, or one named
/// tool. Extra sandbox permissions are always asked.
fn vendor_grant(prompt: &ApprovalPrompt) -> Option<Grant> {
    match prompt.kind.as_str() {
        "command" => Grant::command(&prompt.tool, &prompt.command),
        "file_change" => Some(Grant::kind(&prompt.tool, "file changes")),
        "permissions" => None,
        _ => {
            let name = clip(prompt.command.trim(), 80);
            (!name.is_empty()).then(|| {
                Grant::kind(
                    &format!("{}:{name}", prompt.tool),
                    &format!("`{name}` requests"),
                )
            })
        }
    }
}

async fn request_approval(
    request: &Request<'_>,
    prompt: ApprovalPrompt,
    deny_note: bool,
) -> Result<Answer> {
    let grant = vendor_grant(&prompt);
    let preview = crate::approvals::preview::vendor(
        &request.options.workspace,
        &prompt.kind,
        &prompt.arguments,
    );
    let record = Approval {
        id: String::new(),
        session_id: request.session_id.clone(),
        task_id: request.task_id.clone(),
        tool: prompt.tool,
        arguments: prompt.arguments,
        command: prompt.command,
        reason: prompt.reason,
        pending: true,
        created_at: 0.0,
        expires_at: 0.0,
        preview,
        grant: String::new(),
        note: deny_note,
    };
    let tool = record.tool.clone();
    let mut pending_error = None;
    let answer = request
        .approvals
        .ask(
            record,
            grant.clone(),
            Duration::from_secs(request.config.approval_timeout_sec),
            request.cancel.clone(),
            |record| {
                let mut shown = json!(record);
                crate::redaction::redact_value(&mut shown);
                if let Err(error) = request.events.emit("approval.requested", shown) {
                    pending_error = Some(error);
                    request.cancel.cancel();
                }
            },
        )
        .await?;
    if let Some(error) = pending_error {
        return Err(error);
    }
    if answer.automatic {
        request.events.emit(
            "approval.granted",
            json!({"tool":tool,"job_id":request.job_id,"grant":grant.map(|g|g.label).unwrap_or_default()}),
        )?;
    } else {
        let mut resolved = json!({"tool":"vendor","approved":answer.allow,"job_id":request.job_id,"scope":if answer.for_task {"task"} else {"once"}});
        if let Some(note) = &answer.note {
            resolved["note"] = json!(note);
        }
        crate::redaction::redact_value(&mut resolved);
        request.events.emit("approval.resolved", resolved)?;
    }
    Ok(answer)
}

async fn wait_for_steer(request: &Request<'_>) -> Result<Option<String>> {
    if !request.steer.is_paused() {
        return Ok(None);
    }
    let _parked = request.steer.park()?;
    request.events.emit(
        "agent.paused",
        json!({"job_id":request.job_id,"status":"paused","vendor_agent":true}),
    )?;
    while request.steer.is_paused() {
        tokio::select! {
            _ = request.steer.notify().notified() => {}
            _ = request.cancel.cancelled() => {
                bail!("Task cancelled while paused");
            }
        }
    }
    let note = request
        .steer
        .consume_resume(&std::collections::BTreeMap::new())?;
    if let Some(note) = &note {
        request.events.emit(
            "agent.steered",
            json!({"job_id":request.job_id,"note":crate::tools::truncate(note, 2000)}),
        )?;
    }
    Ok(note)
}

/// Public probe used by tests and doctor-style UI.
pub fn resolve_launch_binary(configured: &str) -> Option<PathBuf> {
    resolve_binary(configured).or_else(|| {
        let path = PathBuf::from(configured);
        path.is_file().then_some(path)
    })
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[tokio::test]
    async fn blocked_input_write_obeys_its_deadline() {
        let (mut writer, _unread) = tokio::io::duplex(8);
        let error = send_lines_bounded(
            &mut writer,
            &["x".repeat(100)],
            &CancellationToken::new(),
            Duration::from_millis(20),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("did not accept input"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn cancellation_takes_priority_over_a_ready_input_write() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = send_lines_bounded(
            &mut writer,
            &["must not be sent".into()],
            &cancel,
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
        drop(writer);
        let mut received = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut received)
            .await
            .unwrap();
        assert!(received.is_empty());
    }

    #[tokio::test]
    async fn oversized_stderr_is_omitted_and_later_diagnostics_still_arrive() {
        let root = tempfile::tempdir().unwrap();
        let paths = crate::paths::AppPaths::isolated(root.path()).unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(&paths.database()).unwrap());
        let session = store.create_session(root.path(), "cli:claude", "").unwrap();
        let session_id = session["id"].as_str().unwrap().to_owned();
        let task_id = store.create_task(&session_id, "diagnostics").unwrap();
        let (sender, _) = tokio::sync::broadcast::channel(8);
        let events = TaskEvents {
            store: store.clone(),
            session_id: session_id.clone(),
            task_id,
            sender,
        };
        let (mut writer, reader) = tokio::io::duplex(4096);
        let writing = tokio::spawn(async move {
            let text = format!(
                "DO_NOT_RETAIN_THIS_DIAGNOSTIC_SECRET {}\nordinary diagnostic\n",
                "x".repeat(MAX_DIAGNOSTIC_BYTES)
            );
            writer.write_all(text.as_bytes()).await.unwrap();
        });
        drain_stderr(reader, events, Default::default()).await;
        writing.await.unwrap();
        let saved = store.events_after(&session_id, 0, None, 100).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(
            saved[0]["payload"]["text"],
            "vendor stderr: omitted oversized diagnostic line"
        );
        assert_eq!(
            saved[1]["payload"]["text"],
            "vendor stderr: ordinary diagnostic"
        );
        assert!(!serde_json::to_string(&saved)
            .unwrap()
            .contains("DIAGNOSTIC_SECRET"));
    }
}

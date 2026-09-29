//! Spawn a vendor CLI in the trusted workspace and translate its stream.
//!
//! The child inherits the user's login environment so the official CLI can
//! read its own credentials. ShadowCode never opens those files, never injects
//! its tools or bubblewrap, and kills the process group on cancel.
use super::{
    adapter_for, clip,
    lines::{BoundedLines, Line, ProtocolLineLimit, MAX_DIAGNOSTIC_BYTES},
    redact, resolve_binary, ApprovalPrompt, CliAdapter, CliAgentsConfig, LaunchOptions, Update,
    Vendor, VendorAnswer, MAX_LINE_BYTES, MAX_MALFORMED_LINES,
};
use crate::{
    approvals::{Answer, Approval, ApprovalHub, Grant},
    events::TaskEvents,
    models::Usage,
    steering::SteerControl,
};
use anyhow::{bail, Context, Result};
use serde_json::json;
#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
use std::{
    collections::VecDeque,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
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

// Per spawned run, across all turns/steering. Do not silently truncate a reply
// into an apparently successful result. These also bound adapter bookkeeping
// and durable event growth caused by many individually valid frames.
const MAX_PROTOCOL_BYTES: usize = 64 * 1024 * 1024;
const MAX_PROTOCOL_FRAMES: usize = 250_000;
const MAX_ASSISTANT_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDERR_WARNINGS: usize = 1000;
const MAX_PENDING_APPROVALS: usize = 32;
const MAX_PENDING_APPROVAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_APPROVAL_DRAIN_FRAMES: usize = 128;

struct PendingApproval<'a> {
    prompt: ApprovalPrompt,
    bytes: usize,
    future: Pin<Box<dyn Future<Output = Result<Answer>> + Send + 'a>>,
    answer: Option<Answer>,
}

enum RunnerInput {
    Line(Result<Option<String>>),
    Tick,
    Decision(Result<Answer>),
    Reconciled,
}

/// Tokio may not have received the reactor notification for bytes already in
/// the pipe. An async Pending therefore cannot alone authorize a reply. The
/// reader owns this descriptor for the entire run; this check never consumes
/// bytes or changes descriptor flags.
#[cfg(unix)]
fn pipe_has_readable_event(fd: RawFd) -> Result<bool> {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut poll, 1, 0) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        // A signal interrupted the check; retry without sending a decision.
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(true);
        }
        return Err(error.into());
    }
    anyhow::ensure!(
        poll.revents & libc::POLLNVAL == 0,
        "Vendor stdout closed during approval reconciliation"
    );
    Ok(poll.revents != 0)
}

#[cfg(not(unix))]
fn pipe_has_readable_event(_: ()) -> Result<bool> {
    bail!("Vendor approval reconciliation cannot verify pipe readiness on this platform")
}

/// A resource/deadline failure must never be mistaken for an unsupported
/// Codex handshake and retried through exec, including before ready().
#[derive(Debug)]
struct RunLimit(String);
impl std::fmt::Display for RunLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for RunLimit {}

struct RunBudget {
    started: Instant,
    user_wait: Duration,
    approval_wait: Option<Instant>,
    active_limit: Duration,
    protocol_bytes: usize,
    protocol_frames: usize,
}
impl RunBudget {
    fn new(seconds: u64) -> Self {
        Self {
            started: Instant::now(),
            user_wait: Duration::ZERO,
            approval_wait: None,
            active_limit: Duration::from_secs(seconds),
            protocol_bytes: 0,
            protocol_frames: 0,
        }
    }
    fn remaining(&self) -> Result<Duration> {
        let waiting = self
            .approval_wait
            .map(|start| start.elapsed())
            .unwrap_or_default();
        let active = self
            .started
            .elapsed()
            .saturating_sub(self.user_wait + waiting);
        self.active_limit.checked_sub(active).filter(|left| !left.is_zero()).ok_or_else(|| {
            RunLimit(format!(
                "Vendor CLI exceeded the {}-second active runtime limit (cli_agents.max_run_time_sec). The run was stopped; existing file changes remain. Continue with a smaller task or adjust this limit in Settings → Advanced.",
                self.active_limit.as_secs()
            )).into()
        })
    }
    fn finish_approval_wait(&mut self) {
        if let Some(started) = self.approval_wait.take() {
            self.user_wait += started.elapsed();
        }
    }
    fn protocol_line(&mut self, bytes: usize) -> Result<()> {
        // Include framing bytes even for empty lines.
        self.protocol_bytes = self.protocol_bytes.saturating_add(bytes.saturating_add(1));
        self.protocol_frames = self.protocol_frames.saturating_add(1);
        if self.protocol_bytes > MAX_PROTOCOL_BYTES || self.protocol_frames > MAX_PROTOCOL_FRAMES {
            return Err(RunLimit(format!(
                "Vendor CLI exceeded the per-run protocol output limit ({} MiB or {} frames). The run was stopped; existing file changes remain. Reduce verbose output or split the task before continuing.",
                MAX_PROTOCOL_BYTES / 1024 / 1024, MAX_PROTOCOL_FRAMES
            )).into());
        }
        Ok(())
    }
    async fn wait_for_user<T>(
        &mut self,
        future: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        let started = Instant::now();
        let result = future.await;
        self.user_wait += started.elapsed();
        result
    }
}

fn append_assistant_text(collected: &mut String, text: &str) -> Result<()> {
    if text.len() > MAX_ASSISTANT_TEXT_BYTES.saturating_sub(collected.len()) {
        return Err(RunLimit(format!(
            "Vendor CLI exceeded the {} MiB assistant text limit for one run. The run was stopped; earlier transcript output and file changes remain. Ask for a shorter reply or split the task before continuing.",
            MAX_ASSISTANT_TEXT_BYTES / 1024 / 1024
        )).into());
    }
    collected.push_str(text);
    Ok(())
}

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
    /// A subagent's approvals are asked in its parent's conversation, with
    /// its name in the reason.
    pub approval_route: Option<crate::subagents::ApprovalRoute>,
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

/// Token counts and cost the vendor reported during a turn. They are kept
/// when the turn fails, hits the plan limit or is cancelled: the vendor
/// already counted them against the user's plan or bill.
#[derive(Clone, Debug, Default)]
pub struct ReportedUsage {
    pub usage: Usage,
    /// The protocol reported token counts (not only a cost).
    pub tokens_reported: bool,
}
impl ReportedUsage {
    /// Anything worth recording: token counts or a cost.
    pub fn any(&self) -> bool {
        self.tokens_reported || self.usage.cost_usd.is_some()
    }
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
    run_reporting_usage(request).await.0
}

/// [`run`], also returning what the vendor reported it used, which is kept
/// whether or not the turn succeeded.
pub async fn run_reporting_usage(request: Request<'_>) -> (Result<RunOutcome>, ReportedUsage) {
    let mut reported = ReportedUsage::default();
    let outcome = run_counted(request, &mut reported).await;
    (outcome, reported)
}

async fn run_counted(request: Request<'_>, reported: &mut ReportedUsage) -> Result<RunOutcome> {
    ensure_ready(request.vendor, &request)?;
    if request.vendor == Vendor::Codex && !codex_app_server_available(&request.options.binary).await
    {
        return run_exec_fallback(
            &request,
            "this Codex CLI has no `app-server` command",
            reported,
        )
        .await;
    }
    let mut reached_ready = false;
    match run_once(
        request.vendor,
        false,
        &request,
        &mut reached_ready,
        reported,
    )
    .await
    {
        Err(error)
            if request.vendor == Vendor::Codex
                && !reached_ready
                && request.images.is_empty()
                && error.downcast_ref::<LimitReached>().is_none()
                && error.downcast_ref::<RunLimit>().is_none()
                && error.downcast_ref::<ProtocolLineLimit>().is_none()
                && !request.cancel.is_cancelled() =>
        {
            run_exec_fallback(&request, &format!("{error:#}"), reported).await
        }
        other => other,
    }
}

async fn run_exec_fallback(
    request: &Request<'_>,
    reason: &str,
    reported: &mut ReportedUsage,
) -> Result<RunOutcome> {
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
    run_once(Vendor::Codex, true, request, &mut reached_ready, reported).await
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
    reported: &mut ReportedUsage,
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
    #[cfg(unix)]
    let stdout_fd = stdout.as_raw_fd();
    #[cfg(not(unix))]
    let stdout_fd = ();
    let stderr = child.stderr.take().context("Vendor CLI stderr missing")?;
    let mut reader = BoundedLines::new(stdout, MAX_LINE_BYTES);
    let sign_in_needed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _stderr_drain = AbortOnDropHandle::new(tokio::spawn(drain_stderr(
        stderr,
        request.events.clone(),
        sign_in_needed.clone(),
    )));
    let mut budget = RunBudget::new(request.config.max_run_time_sec);
    let mut outgoing = adapter.on_start(&request.options);
    outgoing.extend(adapter.prompt(&request.prompt, &request.images)?);
    send_lines(request, &mut stdin, &outgoing, &budget).await?;
    if adapter.one_shot() {
        // One-shot CLIs (`codex exec -`) read the prompt until EOF.
        stdin = None;
    }
    let mut collected = String::new();
    // Counted into the caller's record as they arrive, so a failed,
    // limited or cancelled turn keeps what the vendor already reported.
    let ReportedUsage {
        usage,
        tokens_reported: usage_reported,
    } = reported;
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
    let mut approval: Option<PendingApproval<'_>> = None;
    let mut queued_approvals: VecDeque<(ApprovalPrompt, usize)> = VecDeque::new();
    let mut approval_drain_frames = 0usize;
    loop {
        if request.cancel.is_cancelled() {
            interrupt_vendor(&mut stdin, &mut adapter).await;
            group.terminate();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            group.kill();
            flush_text(request, &message_id, &mut pending_text)?;
            bail!("Task cancelled. The vendor CLI was stopped; its file changes remain on disk.");
        }
        // Test before reading: a ready stdout future can otherwise win forever
        // against a zero-duration timeout during continuous protocol traffic.
        budget.remaining()?;
        if sign_in_needed.load(std::sync::atomic::Ordering::Acquire) {
            group.terminate();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            group.kill();
            bail!("{}", super::acp::ANTIGRAVITY_SIGN_IN);
        }
        if request.steer.is_paused() && !finished {
            if approval.is_some() || !queued_approvals.is_empty() {
                retire_approvals(
                    request,
                    &mut approval,
                    &mut queued_approvals,
                    &mut stdin,
                    &mut adapter,
                    &mut budget,
                    |_| true,
                    "The turn was paused",
                )
                .await?;
                last_line = Instant::now();
            }
            if !interrupted_turn {
                send_lines(request, &mut stdin, &adapter.interrupt(), &budget).await?;
                interrupted_turn = true;
            }
        }
        if approval.is_none() {
            if let Some((prompt, bytes)) = queued_approvals.pop_front() {
                approval = Some(PendingApproval {
                    future: Box::pin(request_approval(
                        request,
                        prompt.clone(),
                        adapter.deny_note(),
                    )),
                    prompt,
                    bytes,
                    answer: None,
                });
                budget.approval_wait = Some(Instant::now());
                approval_drain_frames = 0;
            }
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
        let waiting = budget.approval_wait.is_some();
        let remaining = if waiting {
            stall
        } else {
            stall.saturating_sub(last_line.elapsed())
        }
        .min(budget.remaining()?);
        let ready_answer = approval.as_ref().is_some_and(|p| p.answer.is_some());
        let input = if ready_answer && reader.has_partial_frame() {
            // A split write is still an outstanding frame, even if the kernel
            // pipe is temporarily empty. Its completion remains subject to
            // the active/stall deadline, framing cap and cancellation polls.
            match tokio::time::timeout(remaining.min(poll_interval), reader.next_protocol_line())
                .await
            {
                Ok(line) => RunnerInput::Line(line),
                Err(_) => RunnerInput::Tick,
            }
        } else if ready_answer {
            // A user answer and a changed proposal may both be ready. Consume
            // complete frames already readable before consulting the adapter's
            // permission binding. A continuous producer must fail closed rather
            // than hold this drain open forever. This is a protocol boundary,
            // not proof of what the external vendor actually executes.
            tokio::select! {
                biased;
                // Cooperative scheduler yields are not evidence of an empty
                // pipe. This poll has explicit frame, byte and time bounds.
                line = tokio::task::unconstrained(reader.next_protocol_line()) => RunnerInput::Line(line),
                _ = std::future::ready(()) => RunnerInput::Reconciled,
            }
        } else {
            tokio::select! {
                biased;
                answer = async { approval.as_mut().expect("guarded approval").future.as_mut().await }, if approval.is_some() => RunnerInput::Decision(answer),
                read = tokio::time::timeout(remaining.min(poll_interval), reader.next_protocol_line()) => match read {
                    Ok(line) => RunnerInput::Line(line),
                    Err(_) => RunnerInput::Tick,
                },
            }
        };
        match input {
            RunnerInput::Decision(answer) => {
                approval.as_mut().expect("active approval").answer = Some(answer?);
                budget.finish_approval_wait();
                last_line = Instant::now();
                continue;
            }
            RunnerInput::Reconciled => {
                if request.steer.is_paused() {
                    continue;
                }
                if reader.has_partial_frame() {
                    continue;
                }
                if pipe_has_readable_event(stdout_fd)? {
                    // Let the reactor deliver readiness before polling again.
                    // Active runtime and cancellation are checked each pass.
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    continue;
                }
                let pending = approval.take().expect("ready approval");
                let answer = pending.answer.expect("ready answer");
                let reply = VendorAnswer {
                    allow: answer.allow,
                    for_session: answer.for_task && !answer.automatic,
                    note: answer.note,
                };
                send_lines(
                    request,
                    &mut stdin,
                    &adapter.answer(&pending.prompt.request_id, &reply)?,
                    &budget,
                )
                .await?;
                continue;
            }
            RunnerInput::Line(Ok(None)) => {
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
            RunnerInput::Line(Ok(Some(line))) => {
                budget.protocol_line(line.len())?;
                if ready_answer {
                    approval_drain_frames += 1;
                    if approval_drain_frames > MAX_APPROVAL_DRAIN_FRAMES {
                        return Err(RunLimit("Vendor CLI continued streaming beyond the bounded approval reconciliation limit; no approval was sent".into()).into());
                    }
                }
                last_line = Instant::now();
                let step = adapter.on_line(&line)?;
                // Once a prompt may be written, input failure must not start
                // an exec fallback and risk submitting the task twice.
                *reached_ready |= adapter.ready();
                send_lines(request, &mut stdin, &step.send, &budget).await?;
                let mut saw_protocol = false;
                let user_wait_before = budget.user_wait;
                for update in step.updates {
                    budget.remaining()?;
                    match update {
                        Update::Approval(prompt) => {
                            malformed = 0;
                            saw_protocol = true;
                            flush_text(request, &message_id, &mut pending_text)?;
                            if approval
                                .as_ref()
                                .is_some_and(|p| p.prompt.request_id == prompt.request_id)
                                || queued_approvals
                                    .iter()
                                    .any(|(p, _)| p.request_id == prompt.request_id)
                            {
                                return Err(RunLimit("Vendor CLI reused an outstanding permission request ID; no approval was sent".into()).into());
                            }
                            if request.steer.is_paused()
                                || (request.options.read_only
                                    && matches!(
                                        prompt.kind.as_str(),
                                        "command" | "file_change" | "permissions"
                                    ))
                            {
                                request.events.emit("agent.warning", json!({"text":format!(
                                    "Denied automatically: this task is {}, so {}'s request was declined ({}).",
                                    if request.steer.is_paused() { "paused" } else { "read-only" }, vendor.product_label(), clip(&prompt.command, 300)
                                ),"vendor":vendor.id(),"kind":prompt.kind}))?;
                                send_lines(
                                    request,
                                    &mut stdin,
                                    &adapter.approve(&prompt.request_id, false)?,
                                    &budget,
                                )
                                .await?;
                            } else {
                                let bytes = serde_json::to_vec(&prompt)?.len()
                                    + std::mem::size_of_val(&prompt.tool_identity);
                                let retained = approval.as_ref().map(|p| p.bytes).unwrap_or(0)
                                    + queued_approvals
                                        .iter()
                                        .map(|(_, bytes)| bytes)
                                        .sum::<usize>();
                                if queued_approvals.len() + usize::from(approval.is_some())
                                    >= MAX_PENDING_APPROVALS
                                    || bytes > MAX_PENDING_APPROVAL_BYTES.saturating_sub(retained)
                                {
                                    return Err(RunLimit("Vendor CLI exceeded the pending permission request limit; no approval was sent".into()).into());
                                }
                                queued_approvals.push_back((prompt, bytes));
                            }
                        }
                        Update::Warning(text) => {
                            if text.contains("Ignored a non-JSON")
                                || text.contains("Ignored a non-object")
                                || text.contains("without method or id")
                                || text.contains("without type")
                            {
                                malformed += 1;
                                if malformed > MAX_MALFORMED_LINES {
                                    return Err(RunLimit(format!(
                                        "{} sent too many malformed lines; the runtime was stopped",
                                        vendor.label()
                                    ))
                                    .into());
                                }
                            } else {
                                malformed = 0;
                            }
                            request.events.emit("agent.warning", json!({"text":text}))?;
                        }
                        other => {
                            let retirement = match &other {
                                Update::TurnCompleted { .. }
                                | Update::TurnFailed(_)
                                | Update::LimitReached(_) => Some("The vendor turn ended"),
                                Update::NativeSession { id }
                                    if native_session.as_deref() != Some(id) =>
                                {
                                    Some("The vendor session changed")
                                }
                                Update::ToolCompleted { .. } => {
                                    Some("The proposed tool call already finished")
                                }
                                _ => None,
                            };
                            if let Some(reason) = retirement {
                                retire_approvals(
                                    request,
                                    &mut approval,
                                    &mut queued_approvals,
                                    &mut stdin,
                                    &mut adapter,
                                    &mut budget,
                                    |prompt| match &other {
                                        Update::ToolCompleted { id, .. } => {
                                            prompt.tool_identity
                                                == Some(super::approval_tool_identity(id))
                                        }
                                        _ => true,
                                    },
                                    reason,
                                )
                                .await?;
                            }
                            malformed = 0;
                            saw_protocol = true;
                            let completed_interruption = matches!(
                                &other,
                                Update::TurnCompleted {
                                    interrupted: true,
                                    ..
                                }
                            );
                            apply_update(
                                request,
                                vendor,
                                other,
                                &mut collected,
                                usage,
                                usage_reported,
                                &mut native_session,
                                &mut message_id,
                                &mut pending_text,
                                &mut finished,
                                &mut final_text,
                                &mut stdin,
                                &mut adapter,
                                &mut budget,
                            )
                            .await?;
                            if completed_interruption && !finished {
                                // The inner handler parked and submitted the
                                // follow-up. A later pause must interrupt that
                                // new turn and retire its pending permissions.
                                interrupted_turn = false;
                            }
                        }
                    }
                }
                if budget.user_wait != user_wait_before {
                    last_line = Instant::now();
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
            RunnerInput::Line(Err(error)) => {
                group.kill();
                return Err(error);
            }
            RunnerInput::Tick if !waiting && last_line.elapsed() >= stall => {
                group.terminate();
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                group.kill();
                bail!(
                    "{} produced no output for {} seconds",
                    vendor.label(),
                    request.config.stall_timeout_sec
                );
            }
            RunnerInput::Tick => {
                if request.steer.is_paused() && finished {
                    break;
                }
            }
        }
        if finished && request.steer.is_paused() {
            flush_text(request, &message_id, &mut pending_text)?;
            if let Some(follow_up) = budget.wait_for_user(wait_for_steer(request)).await? {
                finished = false;
                interrupted_turn = false;
                message_id = crate::id();
                outgoing = adapter.prompt(&follow_up, &[])?;
                send_lines(request, &mut stdin, &outgoing, &budget).await?;
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
        usage: usage.clone(),
        usage_reported: *usage_reported,
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
    budget: &RunBudget,
) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let Some(stdin) = stdin.as_mut() else {
        bail!("The vendor CLI input is closed");
    };
    let result = send_lines_bounded(
        stdin,
        lines,
        &request.cancel,
        INPUT_WRITE_TIMEOUT.min(budget.remaining()?),
    )
    .await;
    // Preserve the typed deadline failure so startup cannot fall back to exec.
    if !request.cancel.is_cancelled() {
        budget.remaining()?;
    }
    result
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
    let mut retained = 0;
    while let Ok(Some(line)) = reader.next_line().await {
        let text = match line {
            Line::TooLong => "vendor stderr: omitted oversized diagnostic line".to_owned(),
            Line::Text(line) => {
                let line = redact(line.trim());
                if line.is_empty() {
                    continue;
                }
                // Continue inspecting even after diagnostic retention fills:
                // authentication failure must not become a silent spinner.
                if super::antigravity_server::is_sign_in_prompt(&line) {
                    if !sign_in_needed.swap(true, std::sync::atomic::Ordering::AcqRel) {
                        let _ = events.emit(
                            "agent.warning",
                            json!({"text": super::acp::ANTIGRAVITY_SIGN_IN}),
                        );
                    }
                    continue;
                }
                format!("vendor stderr: {}", clip(&line, 400))
            }
        };
        if retained < MAX_STDERR_WARNINGS {
            retained += 1;
            let _ = events.emit("agent.warning", json!({"text":text}));
        } else if retained == MAX_STDERR_WARNINGS {
            retained += 1;
            let _ = events.emit("agent.warning", json!({
                "text":format!("vendor stderr: further diagnostics omitted after {MAX_STDERR_WARNINGS} lines; the process continues and sign-in failures are still detected")
            }));
        }
        // Keep draining after the cap so a full stderr pipe cannot deadlock
        // normal protocol output. Only diagnostics are omitted, never results.
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

/// Retire stale dialogs before a tool/turn/session boundary. Dropping the
/// ApprovalHub future removes its ticket even if nobody answered the card.
#[allow(clippy::too_many_arguments)]
async fn retire_approvals(
    request: &Request<'_>,
    active: &mut Option<PendingApproval<'_>>,
    queued: &mut VecDeque<(ApprovalPrompt, usize)>,
    stdin: &mut Option<ChildStdin>,
    adapter: &mut Box<dyn CliAdapter>,
    budget: &mut RunBudget,
    retire: impl Fn(&ApprovalPrompt) -> bool,
    reason: &str,
) -> Result<()> {
    let mut retired = Vec::new();
    if active.as_ref().is_some_and(|p| retire(&p.prompt)) {
        let pending = active.take().expect("matched approval");
        drop(pending.future);
        budget.finish_approval_wait();
        request.events.emit(
            "approval.resolved",
            json!({"tool":"vendor","approved":false,"job_id":request.job_id,"reason":reason}),
        )?;
        retired.push(pending.prompt);
    }
    queued.retain(|(prompt, _)| {
        if retire(prompt) {
            retired.push(prompt.clone());
            false
        } else {
            true
        }
    });
    for prompt in retired {
        // A terminal adapter transition may already have removed the RPC.
        // If it still exists, explicitly reject; never reuse an old grant.
        if let Ok(lines) = adapter.approve(&prompt.request_id, false) {
            send_lines(request, stdin, &lines, budget).await?;
        }
    }
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
    budget: &mut RunBudget,
) -> Result<()> {
    match update {
        Update::Text(text) => {
            append_assistant_text(collected, &text)?;
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
        Update::Approval(_) => unreachable!("approvals are handled by the transport loop"),
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
            if let Some(text) = text {
                if !text.is_empty() {
                    append_assistant_text(collected, &text)?;
                    *final_text = Some(text);
                }
            }
            flush_text(request, message_id, pending_text)?;
            // The streamed reply is complete, so the final result can
            // recognise it instead of repeating it.
            request.events.emit(
                "model.stream_end",
                json!({"message_id": message_id, "complete": !interrupted}),
            )?;
            if interrupted && request.steer.is_paused() {
                if let Some(follow_up) = budget.wait_for_user(wait_for_steer(request)).await? {
                    *message_id = crate::id();
                    send_lines(request, stdin, &adapter.prompt(&follow_up, &[])?, budget).await?;
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
                None => super::usage::UsageSnapshot::from_vendor(
                    vendor,
                    &snapshot,
                    None,
                    &request.options.model,
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
    let (session_id, reason) = match &request.approval_route {
        Some(route) => (route.session_id.clone(), route.reason(&prompt.reason)),
        None => (request.session_id.clone(), prompt.reason),
    };
    let record = Approval {
        id: String::new(),
        session_id,
        task_id: request.task_id.clone(),
        tool: prompt.tool,
        arguments: prompt.arguments,
        command: prompt.command,
        reason,
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

    #[test]
    fn protocol_budget_counts_bytes_and_empty_frames_before_adaptation() {
        let mut bytes = RunBudget::new(10);
        bytes.protocol_line(MAX_PROTOCOL_BYTES - 1).unwrap();
        assert!(bytes
            .protocol_line(0)
            .unwrap_err()
            .downcast_ref::<RunLimit>()
            .is_some());
        let mut frames = RunBudget::new(10);
        for _ in 0..MAX_PROTOCOL_FRAMES {
            frames.protocol_line(0).unwrap();
        }
        assert!(frames
            .protocol_line(0)
            .unwrap_err()
            .downcast_ref::<RunLimit>()
            .is_some());
    }

    #[test]
    fn oversized_text_is_refused_without_retaining_a_partial_final_result() {
        let mut text = "x".repeat(MAX_ASSISTANT_TEXT_BYTES - 2);
        append_assistant_text(&mut text, "é").unwrap();
        assert_eq!(text.len(), MAX_ASSISTANT_TEXT_BYTES);
        let error = append_assistant_text(&mut text, "x").unwrap_err();
        assert!(error.downcast_ref::<RunLimit>().is_some());
        assert_eq!(text.len(), MAX_ASSISTANT_TEXT_BYTES);
        assert!(text.ends_with('é'));
    }

    #[test]
    fn only_explicit_user_waits_extend_the_active_deadline() {
        let mut budget = RunBudget::new(2);
        budget.started = Instant::now() - Duration::from_secs(3);
        assert!(budget
            .remaining()
            .unwrap_err()
            .downcast_ref::<RunLimit>()
            .is_some());
        budget.user_wait = Duration::from_secs(2);
        let remaining = budget.remaining().unwrap();
        assert!(remaining > Duration::from_millis(900) && remaining <= Duration::from_secs(1));
    }

    #[tokio::test]
    async fn stderr_retention_is_bounded_and_authentication_detection_survives_the_cap() {
        let root = tempfile::tempdir().unwrap();
        let paths = crate::paths::AppPaths::isolated(root.path()).unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(&paths.database()).unwrap());
        let session = store
            .create_session(root.path(), "cli:antigravity", "")
            .unwrap();
        let session_id = session["id"].as_str().unwrap().to_owned();
        let task_id = store
            .create_task(&session_id, "bounded diagnostics")
            .unwrap();
        let (sender, _) = tokio::sync::broadcast::channel(8);
        let events = TaskEvents {
            store: store.clone(),
            session_id: session_id.clone(),
            task_id,
            sender,
        };
        let mut input = "ordinary diagnostic\n".repeat(MAX_STDERR_WARNINGS + 100);
        input.push_str(
            "Sign in to authenticate the ACP server\nSign in to authenticate the ACP server\n",
        );
        let sign_in = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        drain_stderr(std::io::Cursor::new(input), events, sign_in.clone()).await;
        assert!(sign_in.load(std::sync::atomic::Ordering::Acquire));
        let saved = store.events_after(&session_id, 0, None, 10000).unwrap();
        assert_eq!(saved.len(), MAX_STDERR_WARNINGS + 2);
        assert!(saved[MAX_STDERR_WARNINGS]["payload"]["text"]
            .as_str()
            .unwrap()
            .contains("further diagnostics omitted"));
        assert_eq!(
            saved[MAX_STDERR_WARNINGS + 1]["payload"]["text"],
            super::super::acp::ANTIGRAVITY_SIGN_IN
        );
    }

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

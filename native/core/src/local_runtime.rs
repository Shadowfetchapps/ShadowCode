//! One managed llama-server process. Only one GGUF is loaded at a time.
//!
//! The server binds 127.0.0.1 on a free port, requires a fresh random bearer
//! key per launch (passed through the environment, never argv), serves no web
//! UI, and runs in its own process group with PDEATHSIG so it never outlives
//! ShadowCode. Its stderr is drained into a bounded ring so a chatty server
//! cannot block and so load failures can show the server's own words.
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    io::Read,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
};
use tokio_util::sync::CancellationToken;

pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);
const STDERR_RING_BYTES: usize = 16 * 1024;
const ERROR_TAIL_BYTES: usize = 2000;

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Progress {
    Waiting,
    Loading,
}

/// How to use the GPU for one launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuMode {
    /// All layers on the GPU (`-ngl 999`).
    All,
    /// Let llama.cpp fit what it can (partial offload).
    Auto,
    /// CPU only (`--device none -ngl 0`).
    Off,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaunchSpec {
    pub id: String,
    pub name: String,
    pub binary: PathBuf,
    pub model: PathBuf,
    pub mmproj: Option<PathBuf>,
    pub ctx: u64,
    pub gpu: GpuMode,
    /// Backend reported by `--list-devices` (e.g. `vulkan`, `cpu`).
    pub backend: String,
    /// A bundled, metadata-matched compatibility template, verified on load.
    pub template_profile: Option<crate::local_templates::Profile>,
}

impl LaunchSpec {
    fn same_model(&self, other: &LaunchSpec) -> bool {
        self.binary == other.binary
            && self.model == other.model
            && self.mmproj == other.mmproj
            && self.ctx == other.ctx
            && self.gpu == other.gpu
            && self.backend == other.backend
            && self.template_profile == other.template_profile
    }
}

/// A filesystem snapshot, not a hash of file contents. Unix change time and
/// inode catch same-size rewrites and replacements even when mtime is restored.
/// These observations cannot make a mutable file immutable during inference.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FileIdentity {
    pub path: PathBuf,
    pub bytes: u64,
    pub modified_ns: Option<String>,
    #[cfg(unix)]
    pub device: String,
    #[cfg(unix)]
    pub inode: String,
    #[cfg(unix)]
    pub changed_seconds: i64,
    #[cfg(unix)]
    pub changed_nanoseconds: i64,
}

impl FileIdentity {
    pub fn capture(path: &Path) -> Result<Self> {
        let resolved = std::fs::canonicalize(path)
            .with_context(|| format!("Cannot resolve model or runtime file {}", path.display()))?;
        let meta = std::fs::metadata(&resolved)?;
        ensure!(meta.is_file(), "Not a regular file: {}", path.display());
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            path: resolved,
            bytes: meta.len(),
            modified_ns: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|t| t.as_nanos().to_string()),
            #[cfg(unix)]
            device: meta.dev().to_string(),
            #[cfg(unix)]
            inode: meta.ino().to_string(),
            #[cfg(unix)]
            changed_seconds: meta.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: meta.ctime_nsec(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SourceIdentity {
    pub model: FileIdentity,
    pub runtime: FileIdentity,
    pub projector: Option<FileIdentity>,
}

impl SourceIdentity {
    pub fn capture(spec: &LaunchSpec) -> Result<Self> {
        Ok(Self {
            model: FileIdentity::capture(&spec.model)?,
            runtime: FileIdentity::capture(&spec.binary)?,
            projector: spec
                .mmproj
                .as_deref()
                .map(FileIdentity::capture)
                .transpose()?,
        })
    }

    fn validate(&self, spec: &LaunchSpec) -> Result<()> {
        ensure!(*self == Self::capture(spec)?, "A model, projector, or runtime file changed while preparing the local model. Retry with stable files.");
        Ok(())
    }
}

/// A snapshot of the running server.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub api_key: String,
    pub port: u16,
    pub ctx: u64,
    pub vision: bool,
    pub backend: String,
    pub cpu_fallback: bool,
    pub fallback_reason: Option<String>,
    pub since: f64,
    pub pid: Option<u32>,
    pub provenance: Value,
}

/// Keeps the loaded model in place while a task uses it.
pub struct Lease(Arc<AtomicUsize>, Arc<tokio::sync::Notify>);
impl Drop for Lease {
    fn drop(&mut self) {
        if self.0.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.1.notify_waiters();
        }
    }
}

#[derive(Default)]
struct Ring(VecDeque<u8>);
impl Ring {
    fn push(&mut self, bytes: &[u8]) {
        self.0.extend(bytes);
        let excess = self.0.len().saturating_sub(STDERR_RING_BYTES);
        self.0.drain(..excess);
    }
    fn tail(&self, max: usize) -> String {
        let bytes: Vec<u8> = self.0.iter().copied().collect();
        let start = bytes.len().saturating_sub(max);
        let text = String::from_utf8_lossy(&bytes[start..]).into_owned();
        text.trim().to_owned()
    }
}

struct Server {
    child: Child,
    spec: LaunchSpec,
    info: Loaded,
    stderr: Arc<Mutex<Ring>>,
    leases: Arc<AtomicUsize>,
    sources: SourceIdentity,
}

impl Server {
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
    async fn stop(mut self) {
        terminate(&mut self.child).await;
    }
}

/// SIGTERM to the process group, then SIGKILL after 5 s.
async fn terminate(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        if tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .is_ok()
        {
            return;
        }
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
}

/// The engine's single local model slot.
#[derive(Default)]
pub struct LocalRuntime {
    slot: tokio::sync::Mutex<Option<Server>>,
    /// Cancelled by unload/stop to abort a load in progress.
    abort: Mutex<CancellationToken>,
    snapshot: Mutex<Option<(Loaded, Arc<AtomicUsize>)>>,
    errors: Mutex<HashMap<String, String>>,
    released: Arc<tokio::sync::Notify>,
}

enum LoadFailure {
    /// The process exited before it became ready.
    Exited(String),
    Other(anyhow::Error),
}

impl LocalRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    fn abort_token(&self) -> CancellationToken {
        self.abort
            .lock()
            .map(|t| t.clone())
            .unwrap_or_else(|_| CancellationToken::new())
    }

    fn cancel_loads(&self) {
        if let Ok(mut token) = self.abort.lock() {
            token.cancel();
            *token = CancellationToken::new();
        }
    }

    pub fn last_error(&self, id: &str) -> Option<String> {
        self.errors.lock().ok()?.get(id).cloned()
    }

    pub fn loaded(&self) -> Option<Loaded> {
        self.snapshot.lock().ok()?.as_ref().map(|(l, _)| l.clone())
    }

    pub fn in_use(&self) -> usize {
        self.snapshot
            .lock()
            .ok()
            .and_then(|s| s.as_ref().map(|(_, c)| c.load(Ordering::Acquire)))
            .unwrap_or(0)
    }

    /// Contract `loaded` block (no secrets).
    pub fn loaded_json(&self) -> Option<Value> {
        let loaded = self.loaded()?;
        Some(json!({
            "id": loaded.id,
            "name": loaded.name,
            "port": loaded.port,
            "since": loaded.since,
            "context_tokens": loaded.ctx,
            "backend": loaded.backend,
            "cpu_fallback": loaded.cpu_fallback,
            "fallback_reason": loaded.fallback_reason,
            "vision": loaded.vision,
            "in_use": self.in_use(),
            "provenance": loaded.provenance,
        }))
    }

    /// Start (or reuse) the server for `spec` and lease it. A different model
    /// waits until every current lease is released; same-model callers share.
    /// Cancellable while waiting or loading via `cancel` or [`unload`].
    pub async fn acquire(
        &self,
        spec: LaunchSpec,
        cancel: &CancellationToken,
    ) -> Result<(Loaded, Lease)> {
        self.acquire_with_policy(spec, cancel, true).await
    }

    pub async fn acquire_with_policy(
        &self,
        spec: LaunchSpec,
        cancel: &CancellationToken,
        allow_cpu_fallback: bool,
    ) -> Result<(Loaded, Lease)> {
        self.acquire_with_progress(spec, cancel, allow_cpu_fallback, &|_| Ok(()))
            .await
    }

    pub async fn acquire_with_progress(
        &self,
        spec: LaunchSpec,
        cancel: &CancellationToken,
        allow_cpu_fallback: bool,
        progress: &(dyn Fn(Progress) -> Result<()> + Send + Sync),
    ) -> Result<(Loaded, Lease)> {
        self.acquire_checked_with_progress(spec, cancel, allow_cpu_fallback, progress, None)
            .await
    }

    /// The catalog may pass its already-frozen identities so changes between
    /// compatibility checks and runtime acquisition fail before launch.
    pub async fn acquire_checked_with_progress(
        &self,
        spec: LaunchSpec,
        cancel: &CancellationToken,
        allow_cpu_fallback: bool,
        progress: &(dyn Fn(Progress) -> Result<()> + Send + Sync),
        expected: Option<SourceIdentity>,
    ) -> Result<(Loaded, Lease)> {
        ensure!(!cancel.is_cancelled(), "Model load cancelled");
        let sources = match expected {
            Some(sources) => sources,
            None => SourceIdentity::capture(&spec)?,
        };
        sources.validate(&spec)?;
        let abort = self.abort_token();
        let mut waiting_reported = false;
        loop {
            let released = self.released.notified();
            tokio::pin!(released);
            // Register before inspecting leases to avoid a last-drop wakeup race.
            released.as_mut().enable();
            let mut slot = match self.slot.try_lock() {
                Ok(slot) => slot,
                Err(_) => {
                    if !waiting_reported {
                        progress(Progress::Waiting)?;
                        waiting_reported = true;
                    }
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => bail!("Model load cancelled"),
                        _ = abort.cancelled() => bail!("Model load cancelled because the model was unloaded"),
                        slot = self.slot.lock() => slot,
                    }
                }
            };
            ensure!(
                !cancel.is_cancelled() && !abort.is_cancelled(),
                "Model load cancelled"
            );
            sources.validate(&spec)?;
            if let Some(current) = slot.as_mut() {
                let alive = current.alive();
                if current.spec.same_model(&spec) && current.sources == sources && alive {
                    ensure!(allow_cpu_fallback || !current.info.cpu_fallback, "Comparison cannot reuse an automatic CPU fallback. Unload it and explicitly choose CPU settings or retry the GPU configuration.");
                    current.leases.fetch_add(1, Ordering::AcqRel);
                    return Ok((
                        current.info.clone(),
                        Lease(current.leases.clone(), self.released.clone()),
                    ));
                }
                if !alive {
                    // Keep the crash visible on the row instead of silently restarting.
                    let tail = current
                        .stderr
                        .lock()
                        .map(|r| r.tail(ERROR_TAIL_BYTES))
                        .unwrap_or_default();
                    if let Ok(mut errors) = self.errors.lock() {
                        errors.insert(
                            current.info.id.clone(),
                            format!("llama-server stopped unexpectedly. Last output:\n{tail}"),
                        );
                    }
                }
                let busy = current.leases.load(Ordering::Acquire);
                if busy > 0 && alive {
                    ensure!(current.spec.id != spec.id || (current.sources == sources && current.spec.same_model(&spec)), "A local model file or launch configuration changed while this model is in use. Stop its running tasks before retrying.");
                    drop(slot);
                    if !waiting_reported {
                        progress(Progress::Waiting)?;
                        waiting_reported = true;
                    }
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => bail!("Model load cancelled while waiting for the local runtime"),
                        _ = abort.cancelled() => bail!("Model load cancelled because the model was unloaded"),
                        _ = &mut released => {}
                    }
                    continue;
                }
            }
            progress(Progress::Loading)?;
            if let Some(previous) = slot.take() {
                self.clear_snapshot();
                previous.stop().await;
            }
            let result = self
                .launch_with_fallback(&spec, &sources, cancel, &abort, allow_cpu_fallback)
                .await;
            return match result {
                Ok(server) => {
                    if let Ok(mut errors) = self.errors.lock() {
                        errors.remove(&spec.id);
                    }
                    server.leases.fetch_add(1, Ordering::AcqRel);
                    let lease = Lease(server.leases.clone(), self.released.clone());
                    let info = server.info.clone();
                    if let Ok(mut snapshot) = self.snapshot.lock() {
                        *snapshot = Some((info.clone(), server.leases.clone()));
                    }
                    *slot = Some(server);
                    Ok((info, lease))
                }
                Err(error) => {
                    if let Ok(mut errors) = self.errors.lock() {
                        errors.insert(spec.id.clone(), format!("{error:#}"));
                    }
                    Err(error)
                }
            };
        }
    }

    async fn launch_with_fallback(
        &self,
        spec: &LaunchSpec,
        sources: &SourceIdentity,
        cancel: &CancellationToken,
        abort: &CancellationToken,
        allow_cpu_fallback: bool,
    ) -> Result<Server> {
        match launch(spec, sources, spec.gpu, cancel, abort).await {
            Ok(server) => Ok(server),
            Err(LoadFailure::Exited(first)) if spec.gpu != GpuMode::Off && allow_cpu_fallback => {
                let mut server = match launch(spec, sources, GpuMode::Off, cancel, abort).await {
                    Ok(server) => server,
                    Err(LoadFailure::Exited(second)) => {
                        bail!("{second}\nThe GPU attempt failed first: {first}")
                    }
                    Err(LoadFailure::Other(error)) => return Err(error),
                };
                server.info.cpu_fallback = true;
                server.info.backend = "cpu".into();
                server.info.fallback_reason = Some(first);
                Ok(server)
            }
            Err(LoadFailure::Exited(message)) => Err(anyhow!(message)),
            Err(LoadFailure::Other(error)) => Err(error),
        }
    }

    fn clear_snapshot(&self) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            *snapshot = None;
        }
    }

    /// Stop the loaded model (and abort a load in progress). Refuses while a
    /// task holds the model.
    pub async fn unload(&self) -> Result<bool> {
        if let Some((loaded, leases)) = self.snapshot.lock().ok().and_then(|s| s.clone()) {
            ensure!(
                leases.load(Ordering::Acquire) == 0,
                "A running task is using {}. Stop the task first.",
                loaded.name
            );
        }
        self.cancel_loads();
        let mut slot = self.slot.lock().await;
        if let Some(server) = slot.as_ref() {
            ensure!(
                server.leases.load(Ordering::Acquire) == 0,
                "A running task is using {}. Stop the task first.",
                server.info.name
            );
        }
        let previous = slot.take();
        self.clear_snapshot();
        match previous {
            Some(server) => {
                server.stop().await;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Shutdown: abort loads and stop the server regardless of leases.
    pub async fn stop(&self) {
        self.cancel_loads();
        let previous = self.slot.lock().await.take();
        self.clear_snapshot();
        if let Some(server) = previous {
            server.stop().await;
        }
    }
}

fn free_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("Could not reserve a loopback port")?;
    Ok(listener.local_addr()?.port())
}

/// 32 random bytes from the kernel, hex encoded.
pub fn random_key() -> Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("Could not read random bytes for the local server key")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Server argv (without secrets). Exposed for tests.
pub fn server_args(spec: &LaunchSpec, port: u16, gpu: GpuMode) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-m".into(),
        spec.model.display().to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--no-webui".into(),
        "--jinja".into(),
        "--ctx-size".into(),
        spec.ctx.to_string(),
        "--parallel".into(),
        "1".into(),
    ];
    if let Some(profile) = spec.template_profile {
        // --jinja must precede a literal custom template. No installed model
        // or Ollama template is modified, and no mutable sidecar is loaded.
        args.extend(["--chat-template".into(), profile.template().into()]);
    }
    if let Some(mmproj) = &spec.mmproj {
        args.push("--mmproj".into());
        args.push(mmproj.display().to_string());
    }
    match gpu {
        GpuMode::All => args.extend(["-ngl".into(), "999".into()]),
        GpuMode::Auto => {}
        GpuMode::Off => args.extend(["--device".into(), "none".into(), "-ngl".into(), "0".into()]),
    }
    args
}

/// HTTP client for the local server: loopback only, never through a proxy.
pub fn loopback_client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(1))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

async fn launch(
    spec: &LaunchSpec,
    sources: &SourceIdentity,
    gpu: GpuMode,
    cancel: &CancellationToken,
    abort: &CancellationToken,
) -> std::result::Result<Server, LoadFailure> {
    let other = LoadFailure::Other;
    if cancel.is_cancelled() || abort.is_cancelled() {
        return Err(other(anyhow!("Model load cancelled")));
    }
    sources.validate(spec).map_err(other)?;
    let model_path = spec.model.clone();
    let runtime_path = spec.binary.clone();
    let template_profile = spec.template_profile;
    let metadata = tokio::task::spawn_blocking(move || -> Result<_> {
        let header = crate::local_engine::header(&model_path)?;
        if let Some(profile) = template_profile {
            ensure!(
                crate::local_templates::select(&header) == Some(profile),
                "Model metadata no longer matches the selected local tool template"
            );
        }
        let model = header.provenance();
        let probe = crate::local_engine::probe(&runtime_path);
        ensure!(
            probe.ok,
            "{}",
            probe.error.as_deref().unwrap_or("Runtime probe failed")
        );
        Ok((model, probe))
    });
    // File reads and short-lived CLI probes remain bounded blocking work,
    // but neither a task nor the inference slot waits for them after Stop.
    let (model_provenance, runtime_probe) = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(other(anyhow!("Model load cancelled"))),
        _ = abort.cancelled() => return Err(other(anyhow!("Model load cancelled because the model was unloaded"))),
        result = metadata => result.map_err(|e| other(e.into()))?.map_err(other)?,
    };
    sources.validate(spec).map_err(other)?;
    if !spec.binary.is_file() {
        return Err(other(anyhow!(
            "llama.cpp runtime is missing: {}",
            spec.binary.display()
        )));
    }
    if !spec.model.is_file() {
        return Err(other(anyhow!(
            "GGUF file is missing: {}",
            spec.model.display()
        )));
    }
    let port = free_loopback_port().map_err(other)?;
    let api_key = random_key().map_err(other)?;
    let mut command = Command::new(&spec.binary);
    command
        .args(server_args(spec, port, gpu))
        .env_clear()
        .envs(crate::local_engine::runtime_env(&spec.binary))
        .env("LLAMA_API_KEY", &api_key)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
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
    if cancel.is_cancelled() || abort.is_cancelled() {
        return Err(other(anyhow!("Model load cancelled")));
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to start {}", spec.binary.display()))
        .map_err(other)?;
    let stderr = Arc::new(Mutex::new(Ring::default()));
    if let Some(mut pipe) = child.stderr.take() {
        let ring = stderr.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match pipe.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut ring) = ring.lock() {
                            ring.push(&buf[..n]);
                        }
                    }
                }
            }
        });
    }
    let tail_of = |ring: &Arc<Mutex<Ring>>| {
        ring.lock()
            .map(|r| r.tail(ERROR_TAIL_BYTES))
            .unwrap_or_default()
    };
    let client = loopback_client(Duration::from_secs(2)).map_err(other)?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Let the drain task collect the final lines.
                tokio::time::sleep(Duration::from_millis(100)).await;
                return Err(LoadFailure::Exited(format!(
                    "llama-server exited before it became ready ({status}). Last output:\n{}",
                    tail_of(&stderr)
                )));
            }
            Ok(None) => {}
            Err(error) => return Err(other(error.into())),
        }
        if health(&client, port, &api_key).await {
            break;
        }
        if started.elapsed() > STARTUP_TIMEOUT {
            terminate(&mut child).await;
            return Err(other(anyhow!(
                "llama-server did not become ready within {}s. Last output:\n{}",
                STARTUP_TIMEOUT.as_secs(),
                tail_of(&stderr)
            )));
        }
        tokio::select! {
            _ = cancel.cancelled() => {
                terminate(&mut child).await;
                return Err(other(anyhow!("Model load cancelled")));
            }
            _ = abort.cancelled() => {
                terminate(&mut child).await;
                return Err(other(anyhow!("Model load cancelled because the model was unloaded")));
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
    }
    let props = props(&client, port, &api_key).await;
    if let Err(error) = sources.validate(spec) {
        terminate(&mut child).await;
        return Err(other(error));
    }
    let template_override = match spec.template_profile {
        Some(profile) => match profile.provenance(&props) {
            Some(confirmed) => confirmed,
            None => {
                terminate(&mut child).await;
                return Err(other(anyhow!(
                    "llama-server did not confirm the expected {} tool template; tools were not enabled: {}",
                    profile.id(), profile.mismatch_diagnostic(&props)
                )));
            }
        },
        None => Value::Null,
    };
    let n_ctx = props
        .pointer("/default_generation_settings/n_ctx")
        .and_then(Value::as_u64);
    if let Some(n_ctx) = n_ctx {
        if n_ctx != spec.ctx {
            terminate(&mut child).await;
            return Err(other(anyhow!(
                "llama-server started with a {n_ctx}-token context instead of {}",
                spec.ctx
            )));
        }
    }
    let vision = props
        .pointer("/modalities/vision")
        .and_then(Value::as_bool)
        .unwrap_or(spec.mmproj.is_some())
        && spec.mmproj.is_some();
    let pid = child.id();
    let mode = |gpu| match gpu {
        GpuMode::All => "all",
        GpuMode::Auto => "auto",
        GpuMode::Off => "off",
    };
    Ok(Server {
        info: Loaded {
            id: spec.id.clone(),
            name: spec.name.clone(),
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            api_key,
            port,
            ctx: spec.ctx,
            vision,
            backend: if gpu == GpuMode::Off {
                "cpu".into()
            } else {
                spec.backend.clone()
            },
            cpu_fallback: false,
            fallback_reason: None,
            since: crate::now(),
            pid,
            provenance: json!({
                "schema": 1,
                "identity_kind": "filesystem_metadata",
                "files": sources,
                "model": model_provenance,
                "template_override": template_override,
                "runtime": {
                    "reported_version": runtime_probe.version,
                    "reported_commit": runtime_probe.commit,
                    "reported_generation_defaults": reported_generation_defaults(&props),
                    "reported_chat_template": props.get("chat_template").and_then(Value::as_str).map(crate::gguf::string_identity),
                    "reported_chat_template_tool_use": props.get("chat_template_tool_use").and_then(Value::as_str).map(crate::gguf::string_identity),
                    "reported_tool_capabilities": reported_tool_capabilities(&props),
                },
                "context": {"requested_tokens": spec.ctx, "reported_tokens": n_ctx},
                "gpu": {"requested_mode": mode(spec.gpu), "launch_mode": mode(gpu), "reported_backend": spec.backend},
            }),
        },
        child,
        spec: spec.clone(),
        stderr,
        leases: Arc::new(AtomicUsize::new(0)),
        sources: sources.clone(),
    })
}

/// Runtime template-probe observations, not permission or coding qualification.
/// Missing, malformed and false remain distinct; never copy raw props/template.
fn reported_tool_capabilities(props: &Value) -> Value {
    let raw = props.get("chat_template_caps");
    let caps = raw.and_then(Value::as_object);
    let status = match raw {
        None => "missing",
        Some(Value::Object(_)) => "reported",
        Some(_) => "invalid",
    };
    let mut report = serde_json::Map::new();
    report.insert("field_status".into(), json!(status));
    let mut invalid_fields = Vec::new();
    for key in [
        "supports_tools",
        "supports_tool_calls",
        "supports_parallel_tool_calls",
        "supports_object_arguments",
    ] {
        let field = caps.and_then(|c| c.get(key));
        report.insert(key.into(), json!(field.and_then(Value::as_bool)));
        if field.is_some_and(|v| !v.is_boolean()) {
            invalid_fields.push(key);
        }
    }
    report.insert("invalid_fields".into(), json!(invalid_fields));
    Value::Object(report)
}

fn reported_generation_defaults(props: &Value) -> Value {
    let Some(generation) = props
        .get("default_generation_settings")
        .and_then(Value::as_object)
    else {
        return Value::Null;
    };
    let defaults = generation
        .get("params")
        .and_then(Value::as_object)
        .unwrap_or(generation);
    // Only sampling fields: never persist a server's prompt, raw props, or
    // unsupported values under the guise of effective request settings.
    let mut selected = serde_json::Map::new();
    for key in [
        "seed",
        "temperature",
        "top_k",
        "top_p",
        "min_p",
        "typical_p",
        "repeat_penalty",
        "repeat_last_n",
        "presence_penalty",
        "frequency_penalty",
        "mirostat",
        "mirostat_tau",
        "mirostat_eta",
        "dynatemp_range",
        "dynatemp_exponent",
        "xtc_probability",
        "xtc_threshold",
        "dry_multiplier",
        "dry_base",
        "dry_allowed_length",
        "dry_penalty_last_n",
        "n_predict",
    ] {
        if let Some(value) = defaults.get(key).filter(|v| v.is_number()) {
            selected.insert(key.into(), value.clone());
        }
    }
    if let Some(samplers) = defaults
        .get("samplers")
        .and_then(Value::as_array)
        .filter(|a| a.len() <= 32 && a.iter().all(|v| v.as_str().is_some_and(|s| s.len() <= 64)))
    {
        selected.insert("samplers".into(), json!(samplers));
    }
    Value::Object(selected)
}

async fn health(client: &reqwest::Client, port: u16, key: &str) -> bool {
    client
        .get(format!("http://127.0.0.1:{port}/health"))
        .bearer_auth(key)
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

async fn props(client: &reqwest::Client, port: u16, key: &str) -> Value {
    match client
        .get(format!("http://127.0.0.1:{port}/props"))
        .bearer_auth(key)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            response.json().await.unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_tool_reports_keep_false_missing_and_malformed_distinct() {
        let report = reported_tool_capabilities(&json!({"chat_template_caps": {
            "supports_tools": false,
            "supports_tool_calls": true,
            "supports_parallel_tool_calls": "true",
            "supports_object_arguments": null,
            "private_key": "fake-secret"
        }}));
        assert_eq!(
            report,
            json!({
                "field_status": "reported",
                "supports_tools": false,
                "supports_tool_calls": true,
                "supports_parallel_tool_calls": null,
                "supports_object_arguments": null,
                "invalid_fields": ["supports_parallel_tool_calls", "supports_object_arguments"]
            })
        );
        assert!(!report.to_string().contains("fake-secret"));
        let missing = reported_tool_capabilities(&json!({}));
        assert_eq!(missing["field_status"], "missing");
        assert!(missing["supports_tools"].is_null());
        for invalid in [Value::Null, json!(true), json!(1), json!("true"), json!([])] {
            let report = reported_tool_capabilities(&json!({"chat_template_caps": invalid}));
            assert_eq!(report["field_status"], "invalid");
            assert!(report["supports_tool_calls"].is_null());
        }
        let partial = reported_tool_capabilities(
            &json!({"chat_template_caps": {"supports_tool_calls": false}}),
        );
        assert_eq!(partial["supports_tool_calls"], false);
        assert!(partial["supports_tools"].is_null());
        assert_eq!(partial["invalid_fields"], json!([]));
    }

    #[test]
    fn sampling_provenance_only_records_reported_allowlisted_values() {
        let flat = json!({"default_generation_settings": {"temperature":0.7,"top_k":40,"prompt":"private prompt", "seed":"bad","samplers":["top_k","top_p"]}});
        let expected = json!({"temperature":0.7,"top_k":40,"samplers":["top_k","top_p"]});
        assert_eq!(reported_generation_defaults(&flat), expected);
        assert_eq!(
            reported_generation_defaults(
                &json!({"default_generation_settings":{"params":flat["default_generation_settings"]}})
            ),
            expected
        );
        assert_eq!(reported_generation_defaults(&Value::Null), Value::Null);
    }

    #[cfg(unix)]
    #[test]
    fn file_identity_detects_same_size_in_place_write_with_restored_mtime() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.gguf");
        std::fs::write(&path, b"weights-a").unwrap();
        let before = FileIdentity::capture(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(b"weights-b").unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let after = FileIdentity::capture(&path).unwrap();
        assert_eq!(before.bytes, after.bytes);
        assert_eq!(before.inode, after.inode);
        assert_eq!(before.modified_ns, after.modified_ns);
        assert_ne!(before, after);
    }

    #[test]
    fn reserves_a_loopback_port_and_random_keys() {
        let port = free_loopback_port().unwrap();
        assert!(port > 0);
        let a = random_key().unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, random_key().unwrap());
    }

    #[test]
    fn argv_has_the_security_and_context_flags() {
        let spec = LaunchSpec {
            id: "local:gguf:x".into(),
            name: "x".into(),
            binary: "/bin/true".into(),
            model: "/m/x.gguf".into(),
            mmproj: Some("/m/x.mmproj.gguf".into()),
            ctx: 8192,
            gpu: GpuMode::All,
            backend: "vulkan".into(),
            template_profile: None,
        };
        let args = server_args(&spec, 5555, GpuMode::All).join(" ");
        assert!(args.contains("--host 127.0.0.1 --port 5555"));
        assert!(args.contains("--no-webui --jinja --ctx-size 8192 --parallel 1"));
        assert!(args.contains("--mmproj /m/x.mmproj.gguf"));
        assert!(args.ends_with("-ngl 999"));
        assert!(!args.contains("api-key"), "the key never appears in argv");
        let cpu = server_args(&spec, 5555, GpuMode::Off).join(" ");
        assert!(cpu.ends_with("--device none -ngl 0"));
        let mut changed = spec.clone();
        changed.gpu = GpuMode::Off;
        assert!(!spec.same_model(&changed));
        changed = spec.clone();
        changed.backend = "cuda".into();
        assert!(!spec.same_model(&changed));
        changed = spec.clone();
        let profile = crate::local_templates::Profile::Hermes2ProLlama3;
        changed.template_profile = Some(profile);
        assert!(!spec.same_model(&changed));
        let template_args = server_args(&changed, 5555, GpuMode::All);
        let option = template_args
            .iter()
            .position(|s| s == "--chat-template")
            .unwrap();
        assert_eq!(template_args[option + 1], profile.template());
        assert!(template_args.iter().position(|s| s == "--jinja").unwrap() < option);
        let mut ring = Ring::default();
        ring.push(&vec![b'a'; STDERR_RING_BYTES]);
        ring.push(b"tail");
        assert_eq!(ring.0.len(), STDERR_RING_BYTES);
        assert!(ring.tail(10).ends_with("tail"));
    }
}

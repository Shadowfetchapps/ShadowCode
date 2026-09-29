//! Built-in local GGUF catalog and managed llama.cpp resolution.
//!
//! Everything shown here comes from the files themselves (GGUF metadata read
//! by [`crate::gguf`]) and from running the resolved `llama-server`
//! (`--version`, `--list-devices`). File names never decide compatibility,
//! tool support, or vision. Removing a catalog entry never deletes weights;
//! Ollama store imports reference the store's blobs in place.
use crate::gguf::{self, GgufHeader, MemoryEstimate};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

const MAX_SCAN_FILES: usize = 256;
pub const MANAGED_RELATIVE: &str = ".local/lib/shadowcode";
pub const MANAGED_SERVER: &str = "llama-server";
/// Default upper bound for the server context window (tokens).
pub const DEFAULT_CONTEXT_CAP: u64 = 16384;
/// Never plan a smaller window than this unless the model itself is smaller.
pub const MIN_CONTEXT: u64 = 4096;
const GIB: u64 = 1024 * 1024 * 1024;
/// Head-room left free on the GPU (matches llama.cpp's `--fit-target` default).
const VRAM_MARGIN: u64 = GIB;
/// Memory left to the rest of the system when a model runs on the CPU.
const RAM_MARGIN: u64 = 2 * GIB;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ImportedModel {
    /// Absolute path of the weights (for Ollama, the blob inside the store).
    pub path: String,
    /// Absolute path of the paired vision projector, when the source has one.
    pub mmproj: Option<String>,
    /// Display name (for Ollama, the tag such as `qwen3:14b`).
    pub name: String,
    /// `"ollama"` for store imports.
    pub source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LocalEngineConfig {
    pub directories: Vec<String>,
    pub files: Vec<String>,
    /// Models registered from another store (never copied).
    pub imports: Vec<ImportedModel>,
    /// Files found through a directory that the user removed from the catalog.
    pub excluded: Vec<String>,
    pub llama_binary: String,
    /// Upper bound for the server context window; 0 means the default (16384).
    pub context_size: u64,
    /// Folder of models downloaded from the built-in catalog
    /// (`<data>/local-models`). Set by `Config::load`, never saved: the
    /// finished files there are listed without being registered.
    #[serde(skip)]
    pub downloads: Option<PathBuf>,
}

fn usable_config_path(value: &str) -> bool {
    value.len() <= 4096 && !value.contains(['\n', '\0']) && Path::new(value).is_absolute()
}

impl LocalEngineConfig {
    pub fn from_value(value: &Value) -> Result<Self> {
        let config: Self = serde_json::from_value(value.clone())
            .map_err(|e| anyhow::anyhow!("Invalid local_engine configuration: {e}"))?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.directories.len() <= 32,
            "local_engine.directories holds at most 32 folders"
        );
        ensure!(
            self.files.len() <= 64,
            "local_engine.files holds at most 64 files"
        );
        ensure!(
            self.imports.len() <= 64,
            "local_engine.imports holds at most 64 models"
        );
        ensure!(
            self.excluded.len() <= 1024,
            "local_engine.excluded holds at most 1024 files"
        );
        if self.llama_binary.len() > 1024 || self.llama_binary.contains(['\n', '\0']) {
            bail!("local_engine.llama_binary is not a usable path");
        }
        for path in self
            .directories
            .iter()
            .chain(&self.files)
            .chain(&self.excluded)
        {
            ensure!(
                usable_config_path(path),
                "local_engine paths must be absolute: {path}"
            );
        }
        for import in &self.imports {
            ensure!(
                usable_config_path(&import.path),
                "local_engine.imports: path must be absolute"
            );
            if let Some(mmproj) = &import.mmproj {
                ensure!(
                    usable_config_path(mmproj),
                    "local_engine.imports: mmproj must be absolute"
                );
            }
            ensure!(
                !import.name.trim().is_empty()
                    && import.name.len() <= 256
                    && !import.name.contains(['\n', '\0']),
                "local_engine.imports: name must contain 1-256 bytes"
            );
            ensure!(
                import.source.len() <= 32,
                "local_engine.imports: source must be at most 32 bytes"
            );
        }
        ensure!(
            self.context_size == 0 || (2048..=262_144).contains(&self.context_size),
            "local_engine.context_size must be 0 (default) or between 2048 and 262144"
        );
        Ok(())
    }
    pub fn context_cap(&self) -> u64 {
        if self.context_size == 0 {
            DEFAULT_CONTEXT_CAP
        } else {
            self.context_size
        }
    }
}

/// Why the current catalog policy offers (or does not offer) tool schemas.
/// None of these values certifies runtime parsing or model coding quality.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolsBasis {
    #[default]
    Unknown,
    KnownTemplateProfile,
    TemplateHint,
    NoTemplateHint,
    NoTemplate,
}

/// One local model row. Serialized as the contract's `GgufEntry`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GgufEntry {
    pub id: String,
    pub name: String,
    pub path: String,
    pub bytes: u64,
    /// `"file"`, `"directory"`, or `"ollama"`.
    pub source: String,
    pub architecture: Option<String>,
    pub context_train: Option<u64>,
    /// The context window the server is started with; the engine uses the
    /// same number as its context limit.
    pub context_tokens: u64,
    pub compatible: bool,
    pub reason: String,
    pub vision: bool,
    pub mmproj: Option<String>,
    /// Whether the current policy offers schemas, not verified tool support.
    pub tools: bool,
    pub tools_reason: String,
    #[serde(default)]
    pub tools_basis: ToolsBasis,
    pub memory: MemoryEstimate,
    /// `"gpu"`, `"cpu"`, or `"no"`.
    pub fits: String,
    /// `"ready"`, `"setup_required"`, or `"unavailable"`.
    pub availability: String,
    pub last_error: Option<String>,
    /// The chat template has an `enable_thinking` switch (Qwen3 style).
    #[serde(default)]
    pub thinking_switch: bool,
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Stable id: a hash of the canonical path. Never derived from a display name.
pub fn entry_id(path: &Path) -> String {
    format!(
        "local:gguf:{}",
        crate::workspace::hash(canonical(path).to_string_lossy().as_bytes())
    )
}

// ---------------------------------------------------------------------------
// GGUF header cache (filesystem identity, including inode + ctime on Unix)
// ---------------------------------------------------------------------------

type FileKey = crate::local_runtime::FileIdentity;

fn file_key(path: &Path) -> Option<FileKey> {
    FileKey::capture(path).ok()
}

type HeaderCache = HashMap<PathBuf, (FileKey, Arc<GgufHeader>)>;
static HEADERS: LazyLock<Mutex<HeaderCache>> = LazyLock::new(Default::default);

pub fn header(path: &Path) -> Result<Arc<GgufHeader>> {
    let key = file_key(path).with_context(|| format!("File not found: {}", path.display()))?;
    if let Ok(cache) = HEADERS.lock() {
        if let Some((cached, header)) = cache.get(path) {
            if *cached == key {
                return Ok(header.clone());
            }
        }
    }
    let parsed = Arc::new(gguf::read_header(path)?);
    ensure!(
        file_key(path).as_ref() == Some(&key),
        "GGUF changed while reading its header: {}",
        path.display()
    );
    if let Ok(mut cache) = HEADERS.lock() {
        if cache.len() > 512 {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), (key, parsed.clone()));
    }
    Ok(parsed)
}

/// Why a GGUF file is not a chat model; `None` for a usable text model.
pub fn not_a_model(header: &GgufHeader) -> Option<&'static str> {
    if header.is_projector() {
        Some("This is a vision projector (mmproj), not a model. Add the model file; its projector is paired automatically.")
    } else if header.is_vocab_only() {
        Some("This GGUF contains only a tokenizer vocabulary (no weights).")
    } else if header.is_embedding_model() {
        Some("This is an embedding model; it cannot chat.")
    } else {
        None
    }
}

fn is_projector_file(path: &Path) -> bool {
    header(path).is_ok_and(|h| h.is_projector())
}

/// The projector's output width must match the model's embedding width when
/// both files record it; otherwise the pairing is wrong.
fn projector_matches(model: &GgufHeader, projector: &Path) -> bool {
    let Ok(proj) = header(projector) else {
        return false;
    };
    if !proj.is_projector() {
        return false;
    }
    let width = ["clip.vision.projection_dim", "clip.projection_dim"]
        .iter()
        .find_map(|k| proj.u64(k));
    match (width, model.arch_u64("embedding_length")) {
        (Some(p), Some(m)) => p == m,
        _ => true,
    }
}

/// Pair a projector by exact stem patterns, else a single projector in the
/// same folder whose width matches the model.
pub fn find_projector(model_path: &Path, model: &GgufHeader) -> Option<PathBuf> {
    let dir = model_path.parent()?;
    let stem = model_path.file_stem()?.to_str()?;
    for name in [
        format!("{stem}.mmproj.gguf"),
        format!("{stem}-mmproj.gguf"),
        format!("mmproj-{stem}.gguf"),
        format!("{stem}.mmproj"),
    ] {
        let candidate = dir.join(name);
        if candidate.is_file() && projector_matches(model, &candidate) {
            return Some(candidate);
        }
    }
    // Folder fallback: exactly one projector and exactly one model file.
    let read = fs::read_dir(dir).ok()?;
    let files: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("gguf") && p.is_file())
        .take(64)
        .collect();
    let (projectors, models): (Vec<&PathBuf>, Vec<&PathBuf>) =
        files.iter().partition(|p| is_projector_file(p));
    match (projectors.as_slice(), models.as_slice()) {
        ([single], [only]) if *only == model_path && projector_matches(model, single) => {
            Some((*single).clone())
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Runtime resolution and readiness
// ---------------------------------------------------------------------------

pub fn managed_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(MANAGED_RELATIVE))
}

/// `usr/lib/shadowcode` next to the executable (AppImage `usr/bin/..`, deb
/// `/usr/bin/..`).
pub fn bundled_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("../lib/shadowcode"))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommitRecord {
    pub commit: Option<String>,
    pub backend: Option<String>,
    pub built: Option<String>,
}

pub fn read_commit(dir: &Path) -> Option<CommitRecord> {
    let text = fs::read_to_string(dir.join("COMMIT")).ok()?;
    let mut record = CommitRecord::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = Some(value.trim().to_owned()).filter(|v| !v.is_empty());
        match key.trim() {
            "commit" => record.commit = value,
            "backend" => record.backend = value,
            "built" => record.built = value,
            _ => {}
        }
    }
    Some(record)
}

fn not_llama_cli(path: &Path) -> bool {
    !path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("llama-cli"))
}

/// Candidate `llama-server` binaries in preference order with their origin.
///
/// Order: an explicitly configured path; `SHADOWCODE_LLAMA_SERVER` (an
/// explicit override); the runtime bundled next to the executable when the
/// managed directory is missing or older (by COMMIT `built=`);
/// `~/.local/lib/shadowcode`; the bundle. Never `llama-cli`, never a bare
/// PATH lookup.
pub fn runtime_candidates_with(
    configured: &str,
    bundled: Option<PathBuf>,
    managed: Option<PathBuf>,
    env_server: Option<PathBuf>,
) -> Vec<(PathBuf, &'static str)> {
    let mut out: Vec<(PathBuf, &'static str)> = Vec::new();
    let configured = configured.trim();
    if !configured.is_empty() {
        out.push((PathBuf::from(configured), "other"));
    }
    if let Some(env) = env_server {
        out.push((env, "other"));
    }
    let bundle = bundled.filter(|d| d.join(MANAGED_SERVER).is_file());
    let managed = managed.filter(|d| d.join(MANAGED_SERVER).is_file());
    let bundle_first = match (&bundle, &managed) {
        (Some(_), None) => true,
        (Some(b), Some(m)) => {
            let built = |d: &Path| read_commit(d).and_then(|c| c.built);
            match (built(b), built(m)) {
                (Some(b), Some(m)) => b > m,
                (Some(_), None) => true,
                _ => false,
            }
        }
        _ => false,
    };
    if bundle_first {
        if let Some(b) = &bundle {
            out.push((b.join(MANAGED_SERVER), "bundled"));
        }
    }
    if let Some(m) = &managed {
        out.push((m.join(MANAGED_SERVER), "managed"));
    }
    if !bundle_first {
        if let Some(b) = &bundle {
            out.push((b.join(MANAGED_SERVER), "bundled"));
        }
    }
    out.retain(|(p, _)| p.is_file() && not_llama_cli(p));
    let mut seen = HashSet::new();
    out.retain(|(p, _)| seen.insert(canonical(p)));
    out
}

pub fn runtime_candidates(configured: &str) -> Vec<(PathBuf, &'static str)> {
    runtime_candidates_with(
        configured,
        bundled_dir(),
        managed_dir(),
        std::env::var_os("SHADOWCODE_LLAMA_SERVER").map(PathBuf::from),
    )
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// Parse `llama-server --list-devices`:
/// `  Vulkan0: NVIDIA GeForce RTX 5060 Ti (16557 MiB, 14807 MiB free)`.
pub fn parse_devices(text: &str) -> Vec<Device> {
    let pattern =
        regex::Regex::new(r"^\s*([A-Za-z_\-]+\d*):\s+(.+?)\s+\((\d+) MiB,\s*(\d+) MiB free\)\s*$")
            .expect("device pattern");
    text.lines()
        .filter_map(|line| {
            let caps = pattern.captures(line)?;
            let mib = |i: usize| caps[i].parse::<u64>().ok().map(|v| v * 1024 * 1024);
            Some(Device {
                id: caps[1].to_owned(),
                name: caps[2].to_owned(),
                total_bytes: mib(3)?,
                free_bytes: mib(4)?,
            })
        })
        .collect()
}

/// `version: 0.4.1-dev (build 1, commit 18f9f7bef)` → (version, commit).
pub fn parse_version(text: &str) -> Option<(String, Option<String>)> {
    let line = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("version:"))?;
    let line = line.trim();
    let version = line.split_whitespace().next()?.to_owned();
    let commit = line
        .split("commit ")
        .nth(1)
        .map(|c| c.trim_end_matches(')').trim().to_owned());
    Some((version, commit))
}

fn backend_of(devices: &[Device]) -> String {
    devices
        .first()
        .map(|d| {
            d.id.trim_end_matches(|c: char| c.is_ascii_digit())
                .to_ascii_lowercase()
        })
        .unwrap_or_else(|| "cpu".into())
}

/// Environment for runtime processes: a small allowlist plus the runtime's
/// own library directory. Nothing else from the parent leaks in (no proxies,
/// no `LLAMA_ARG_*` overrides).
pub fn runtime_env(binary: &Path) -> Vec<(String, std::ffi::OsString)> {
    let mut env = Vec::new();
    for name in [
        "PATH",
        "HOME",
        "USER",
        "LANG",
        "LC_ALL",
        "TMPDIR",
        "XDG_RUNTIME_DIR",
        "XDG_CACHE_HOME",
        "VK_ICD_FILENAMES",
        "VK_DRIVER_FILES",
        "GGML_VK_VISIBLE_DEVICES",
        "CUDA_VISIBLE_DEVICES",
    ] {
        if let Some(value) = std::env::var_os(name) {
            env.push((name.to_owned(), value));
        }
    }
    if let Some(dir) = binary.parent() {
        env.push(("LD_LIBRARY_PATH".into(), dir.as_os_str().to_owned()));
    }
    env
}

/// The child has exited but is not reaped yet (`WNOWAIT`), so its process
/// group id still belongs to it.
#[cfg(unix)]
fn exited_unreaped(pid: u32) -> bool {
    // SAFETY: waitid only writes the zeroed siginfo it is given.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        ) == 0
            && info.si_pid() != 0
    }
}
#[cfg(unix)]
fn kill_group(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: signals only the probe's own process group.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

/// Run a runtime command with a bounded wait; returns (success, combined output).
fn run_probe(binary: &Path, args: &[&str], timeout: Duration) -> Result<(bool, String)> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env_clear()
        .envs(runtime_env(binary))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Could not run {}", binary.display()))?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut text = Vec::new();
            if let Some(pipe) = pipe {
                let _ = pipe.take(256 * 1024).read_to_end(&mut text);
            }
            let _ = sender.send(text);
        });
        receiver
    };
    let out = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        // Anything the runtime left behind in its process group (a wrapper
        // script's child) is stopped while the leader is not yet reaped, so
        // its id cannot have been reused, and cannot hold the pipes open.
        #[cfg(unix)]
        if exited_unreaped(child.id()) {
            kill_group(child.id());
            break Some(child.wait()?);
        }
        #[cfg(not(unix))]
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() > timeout {
            #[cfg(unix)]
            kill_group(child.id());
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // A process outside the group could still hold a pipe: never wait on it
    // for long.
    let collect = |receiver: std::sync::mpsc::Receiver<Vec<u8>>| {
        receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_default()
    };
    let mut text = String::from_utf8_lossy(&collect(out)).into_owned();
    text.push_str(&String::from_utf8_lossy(&collect(err)));
    match status {
        Some(status) => Ok((status.success(), text)),
        None => bail!(
            "{} {} did not finish within {}s",
            binary.display(),
            args.join(" "),
            timeout.as_secs()
        ),
    }
}

#[derive(Clone, Debug)]
pub struct Probe {
    pub ok: bool,
    pub version: Option<String>,
    pub commit: Option<String>,
    pub devices: Vec<Device>,
    pub devices_known: bool,
    pub error: Option<String>,
    pub architectures: Option<Arc<HashSet<String>>>,
    pub record: Option<CommitRecord>,
}

static PROBES: LazyLock<Mutex<HashMap<PathBuf, (FileKey, Probe)>>> =
    LazyLock::new(Default::default);

fn tail(text: &str, max: usize) -> String {
    let text = text.trim();
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

/// Ready only after `<llama-server> --version` succeeds. Results are cached
/// per filesystem identity, so replacing the runtime re-probes.
pub fn probe(binary: &Path) -> Probe {
    let key = file_key(binary);
    if let (Some(key), Ok(cache)) = (&key, PROBES.lock()) {
        if let Some((cached, probe)) = cache.get(binary) {
            if cached == key {
                return probe.clone();
            }
        }
    }
    let dir = binary.parent().map(Path::to_path_buf).unwrap_or_default();
    let architectures = fs::read_to_string(dir.join("architectures.txt"))
        .ok()
        .map(|text| {
            Arc::new(
                text.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(str::to_owned)
                    .collect::<HashSet<_>>(),
            )
        })
        .filter(|set| !set.is_empty());
    let record = read_commit(&dir);
    let mut probe = Probe {
        ok: false,
        version: None,
        commit: None,
        devices: Vec::new(),
        devices_known: false,
        error: None,
        architectures,
        record,
    };
    match run_probe(binary, &["--version"], Duration::from_secs(15)) {
        Ok((true, text)) => {
            probe.ok = true;
            if let Some((version, commit)) = parse_version(&text) {
                probe.version = Some(version);
                probe.commit = commit;
            }
        }
        Ok((false, text)) => {
            probe.error = Some(format!(
                "{} --version failed: {}",
                binary.display(),
                tail(&text, 600)
            ))
        }
        Err(error) => probe.error = Some(format!("{error:#}")),
    }
    if probe.ok {
        if let Ok((true, text)) = run_probe(binary, &["--list-devices"], Duration::from_secs(20)) {
            probe.devices = parse_devices(&text);
            probe.devices_known = true;
        }
    }
    if file_key(binary) != key {
        probe.ok = false;
        probe.error =
            Some("Runtime file changed while probing it. Retry with stable files.".into());
        return probe;
    }
    if let (Some(key), Ok(mut cache)) = (key, PROBES.lock()) {
        cache.insert(binary.to_path_buf(), (key, probe.clone()));
    }
    probe
}

#[derive(Clone, Debug)]
pub struct Runtime {
    /// `"ready"`, `"setup_required"`, or `"unavailable"`.
    pub state: &'static str,
    pub path: Option<PathBuf>,
    pub origin: Option<&'static str>,
    pub probe: Option<Probe>,
    pub detail: String,
}

impl Runtime {
    pub fn ready(&self) -> bool {
        self.state == "ready"
    }
    pub fn devices(&self) -> &[Device] {
        self.probe.as_ref().map(|p| &p.devices[..]).unwrap_or(&[])
    }
    pub fn gpu(&self) -> Option<&Device> {
        self.devices().iter().max_by_key(|d| d.total_bytes)
    }
    pub fn backend(&self) -> String {
        match &self.probe {
            Some(p) if p.devices_known => backend_of(&p.devices),
            _ => "unknown".into(),
        }
    }
    pub fn to_json(&self) -> Value {
        let probe = self.probe.as_ref();
        json!({
            "state": self.state,
            "path": self.path,
            "origin": self.origin,
            "version": probe.and_then(|p| p.version.clone()),
            "backend": probe.and_then(|p| p.record.as_ref()).and_then(|r| r.backend.clone()).unwrap_or_else(|| self.backend()),
            "commit": probe.and_then(|p| p.commit.clone().or_else(|| p.record.as_ref().and_then(|r| r.commit.clone()))),
            "detail": self.detail,
        })
    }
}

pub fn runtime(configured: &str) -> Runtime {
    runtime_from(runtime_candidates(configured))
}

pub fn runtime_from(candidates: Vec<(PathBuf, &'static str)>) -> Runtime {
    if candidates.is_empty() {
        return Runtime {
            state: "setup_required",
            path: None,
            origin: None,
            probe: None,
            detail: "The llama.cpp runtime is not installed. Reinstall ShadowCode: the AppImage and .deb include it. Models are never downloaded automatically.".into(),
        };
    }
    let mut first_failure: Option<Runtime> = None;
    for (path, origin) in candidates {
        let probe = probe(&path);
        if probe.ok {
            let detail = format!(
                "llama.cpp {}{} · {}",
                probe.version.as_deref().unwrap_or("unknown version"),
                probe
                    .commit
                    .as_deref()
                    .map(|c| format!(" ({c})"))
                    .unwrap_or_default(),
                match probe.devices.first() {
                    Some(d) => format!("{} {}", d.id, d.name),
                    None if probe.devices_known => "CPU only (no GPU device found)".into(),
                    None => "devices unknown".into(),
                }
            );
            return Runtime {
                state: "ready",
                path: Some(path),
                origin: Some(origin),
                probe: Some(probe),
                detail,
            };
        }
        if first_failure.is_none() {
            let detail = format!(
                "The llama.cpp runtime at {} does not start: {}",
                path.display(),
                probe.error.clone().unwrap_or_default()
            );
            first_failure = Some(Runtime {
                state: "unavailable",
                path: Some(path),
                origin: Some(origin),
                probe: Some(probe),
                detail,
            });
        }
    }
    first_failure.expect("at least one candidate")
}

pub fn read_meminfo() -> u64 {
    let Ok(text) = fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok());
            return kb.unwrap_or(0).saturating_mul(1024);
        }
    }
    0
}

pub fn hardware_json(runtime: &Runtime) -> Value {
    let ram = read_meminfo();
    let gpu = runtime.gpu();
    let backend = runtime.backend();
    let devices: Vec<String> = runtime
        .devices()
        .iter()
        .map(|d| {
            format!(
                "{}: {} ({} MiB)",
                d.id,
                d.name,
                d.total_bytes / (1024 * 1024)
            )
        })
        .collect();
    let detail = match (gpu, backend.as_str()) {
        (Some(g), _) => format!(
            "{} with {:.1} GB VRAM via {}; {:.1} GB RAM",
            g.name,
            gb(g.total_bytes),
            g.id.trim_end_matches(|c: char| c.is_ascii_digit()),
            gb(ram)
        ),
        (None, "cpu") => format!("CPU only; {:.1} GB RAM", gb(ram)),
        _ => format!(
            "{:.1} GB RAM; GPU unknown until the runtime is installed",
            gb(ram)
        ),
    };
    json!({
        "cpu_cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        "ram_bytes": ram,
        "gpu": gpu.map(|g| g.name.clone()),
        "vram_bytes": gpu.map(|g| g.total_bytes),
        "backend": backend,
        "devices": devices,
        "detail": detail,
    })
}

fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1_000_000_000.0
}

// ---------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------

/// What the planner needs to know about this computer.
#[derive(Clone, Debug, Default)]
pub struct Budget {
    pub runtime_ready: bool,
    pub gpu_bytes: Option<u64>,
    pub ram_bytes: u64,
    pub context_cap: u64,
    pub architectures: Option<Arc<HashSet<String>>>,
}

impl Budget {
    pub fn from_runtime(runtime: &Runtime, config: &LocalEngineConfig) -> Self {
        Self {
            runtime_ready: runtime.ready(),
            gpu_bytes: runtime.gpu().map(|g| g.total_bytes),
            ram_bytes: read_meminfo(),
            context_cap: config.context_cap(),
            architectures: runtime.probe.as_ref().and_then(|p| p.architectures.clone()),
        }
    }
}

/// Largest context (≤ cap, ≤ trained, ≥ 4096 unless the model is smaller)
/// whose estimate fits the GPU, else the RAM; `fits` is `gpu|cpu|no`.
pub fn plan_context(
    header: &GgufHeader,
    weights: u64,
    projector: u64,
    budget: &Budget,
) -> (u64, MemoryEstimate, &'static str) {
    let trained = header.arch_u64("context_length").filter(|v| *v > 0);
    let max = trained
        .unwrap_or(budget.context_cap)
        .min(budget.context_cap)
        .max(256);
    let floor = MIN_CONTEXT.min(max);
    let mut sizes = vec![max];
    let mut next = max / 2;
    while next > floor {
        sizes.push(next);
        next /= 2;
    }
    if *sizes.last().unwrap_or(&0) != floor {
        sizes.push(floor);
    }
    let targets = [
        (
            budget.gpu_bytes.map(|v| v.saturating_sub(VRAM_MARGIN)),
            "gpu",
        ),
        (Some(budget.ram_bytes.saturating_sub(RAM_MARGIN)), "cpu"),
    ];
    for (limit, label) in targets {
        let Some(limit) = limit.filter(|v| *v > 0) else {
            continue;
        };
        for &ctx in &sizes {
            let estimate = gguf::estimate_memory(header, weights, projector, ctx);
            if estimate.total_bytes <= limit {
                return (ctx, estimate, label);
            }
        }
    }
    (
        floor,
        gguf::estimate_memory(header, weights, projector, floor),
        "no",
    )
}

/// The name a file-based model shows in the picker: the model's own
/// `general.name` (plus the quantization from the file name, so two
/// quantizations of one model stay distinguishable), else the file stem.
fn display_name(header: &GgufHeader, path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("local-model");
    let Some(name) = header
        .str("general.name")
        .map(|n| n.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|n| !n.is_empty() && n.chars().count() <= 80)
    else {
        return stem.to_owned();
    };
    // Quantization tag from the file name, e.g. `Qwen3-14B-Q4_K_M.gguf`.
    let quant =
        regex::Regex::new(r"(?i)(?:^|[-._])(I?Q\d(?:_[0-9A-Z]{1,2}){0,2}|BF16|F16|F32)(?:$|[-.])")
            .ok()
            .and_then(|re| re.captures_iter(stem).last())
            .map(|c| c[1].to_ascii_uppercase());
    match quant {
        Some(q) if !name.to_ascii_uppercase().contains(&q) => format!("{name} · {q}"),
        _ => name,
    }
}

/// How a catalog row was found.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub path: PathBuf,
    pub name: Option<String>,
    /// `Some(Some(p))` explicit projector, `Some(None)` explicitly none,
    /// `None` pair automatically.
    pub mmproj: Option<Option<PathBuf>>,
    pub source: &'static str,
}

fn unusable_entry(candidate: &Candidate, reason: String) -> GgufEntry {
    GgufEntry {
        id: entry_id(&candidate.path),
        name: candidate.name.clone().unwrap_or_else(|| {
            candidate
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("local-model")
                .to_owned()
        }),
        path: candidate.path.display().to_string(),
        bytes: fs::metadata(&candidate.path).map(|m| m.len()).unwrap_or(0),
        source: candidate.source.into(),
        architecture: None,
        context_train: None,
        context_tokens: 0,
        compatible: false,
        reason,
        vision: false,
        mmproj: None,
        tools: false,
        tools_reason: "Unknown".into(),
        tools_basis: ToolsBasis::Unknown,
        memory: MemoryEstimate {
            weights_bytes: 0,
            kv_cache_bytes: 0,
            compute_bytes: 0,
            projector_bytes: 0,
            overhead_bytes: 0,
            total_bytes: 0,
            context_tokens: 0,
        },
        fits: "no".into(),
        availability: "unavailable".into(),
        last_error: None,
        thinking_switch: false,
    }
}

/// Inspect a candidate. `Err` means "not a chat model" (projector, vocab,
/// embedding, not GGUF); such files are left out of directory listings.
pub fn inspect(candidate: &Candidate, budget: &Budget) -> Result<GgufEntry> {
    let path = &candidate.path;
    ensure!(path.is_file(), "File not found: {}", path.display());
    ensure!(gguf::is_gguf(path), "Not a GGUF file (missing GGUF magic)");
    let header = header(path)?;
    if let Some(reason) = not_a_model(&header) {
        bail!("{reason}");
    }
    let bytes = fs::metadata(path)?.len();
    let architecture = header.architecture().map(str::to_owned);
    let context_train = header.arch_u64("context_length");
    let mut notes = Vec::new();
    let mmproj = match &candidate.mmproj {
        Some(Some(p)) if p.is_file() && is_projector_file(p) => Some(p.clone()),
        Some(Some(p)) => {
            notes.push(format!(
                "Vision projector {} is missing or unreadable; text only.",
                p.display()
            ));
            None
        }
        Some(None) => None,
        None => find_projector(path, &header),
    };
    let projector_bytes = mmproj
        .as_ref()
        .and_then(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);
    let (compatible, mut reason) = match (&architecture, &budget.architectures) {
        (None, _) => (false, "GGUF has no general.architecture".to_owned()),
        (Some(_), _) if !header.has_tensor("token_embd.weight") => (
            false,
            "GGUF has no token embedding tensor (token_embd.weight)".to_owned(),
        ),
        (Some(arch), Some(list)) if !list.contains(arch) => {
            (false, format!("unsupported architecture {arch}"))
        }
        _ => (true, String::new()),
    };
    let (context_tokens, memory, fits) = plan_context(&header, bytes, projector_bytes, budget);
    let template_profile = crate::local_templates::select(&header);
    let tools = template_profile.is_some() || header.template_mentions_tools();
    let (tools_basis, tools_reason) = match (template_profile, header.chat_template()) {
        (Some(_), _) => (
            ToolsBasis::KnownTemplateProfile,
            "Bundled Hermes tool template · verified when the model loads",
        ),
        (None, None) => (
            ToolsBasis::NoTemplate,
            "No chat template in the file · Chat only",
        ),
        (None, Some(_)) if tools => (
            ToolsBasis::TemplateHint,
            "Template mentions tools · file hint only",
        ),
        (None, Some(_)) => (
            ToolsBasis::NoTemplateHint,
            "No tool hint in the retained template · Chat only under current policy",
        ),
    };
    let tools_reason = tools_reason.to_owned();
    let availability = if !compatible || fits == "no" {
        if compatible {
            reason = format!(
                "Needs ≈{:.1} GB at a {}-token context; this computer has {:.1} GB RAM{}",
                gb(memory.total_bytes),
                context_tokens,
                gb(budget.ram_bytes),
                budget
                    .gpu_bytes
                    .map(|v| format!(" and {:.1} GB VRAM", gb(v)))
                    .unwrap_or_default()
            );
        }
        "unavailable"
    } else if !budget.runtime_ready {
        reason = "The llama.cpp runtime is not ready; see Settings › Local models.".into();
        "setup_required"
    } else {
        reason = format!(
            "{} · {}-token context · ≈{:.1} GB {}",
            architecture.as_deref().unwrap_or("gguf"),
            context_tokens,
            gb(memory.total_bytes),
            if fits == "gpu" {
                "in GPU memory"
            } else {
                "in RAM (CPU)"
            }
        );
        "ready"
    };
    if !notes.is_empty() {
        reason = format!("{reason}. {}", notes.join(" "));
    }
    Ok(GgufEntry {
        id: entry_id(path),
        name: candidate
            .name
            .clone()
            .unwrap_or_else(|| display_name(&header, path)),
        path: path.display().to_string(),
        bytes,
        source: candidate.source.into(),
        architecture,
        context_train,
        context_tokens,
        compatible,
        reason,
        vision: mmproj.is_some(),
        mmproj: mmproj.map(|p| p.display().to_string()),
        tools,
        tools_reason,
        tools_basis,
        memory,
        fits: fits.into(),
        availability: availability.into(),
        last_error: None,
        thinking_switch: header.template_has_thinking_switch(),
    })
}

/// Validate a file the user wants to add. Returns a clear refusal for
/// projectors, vocab-only, embedding, and non-GGUF files.
pub fn check_addable(path: &Path) -> Result<()> {
    ensure!(path.is_file(), "File not found: {}", path.display());
    ensure!(gguf::is_gguf(path), "Not a GGUF file (missing GGUF magic)");
    let header = header(path)?;
    if let Some(reason) = not_a_model(&header) {
        bail!("{reason}");
    }
    Ok(())
}

pub fn candidates(config: &LocalEngineConfig) -> Vec<Candidate> {
    let excluded: HashSet<PathBuf> = config
        .excluded
        .iter()
        .map(|p| canonical(Path::new(p)))
        .collect();
    let mut out = Vec::new();
    // Finished catalog downloads, under the catalog's name (some files carry
    // a placeholder `general.name`).
    if let Some(dir) = &config.downloads {
        for (model, path) in crate::local_downloads::installed(dir) {
            out.push(Candidate {
                path,
                name: Some(model.name.to_owned()),
                mmproj: Some(None),
                source: "download",
            });
        }
    }
    for import in &config.imports {
        out.push(Candidate {
            path: PathBuf::from(&import.path),
            name: Some(import.name.clone()),
            mmproj: Some(import.mmproj.as_ref().map(PathBuf::from)),
            source: if import.source == "ollama" {
                "ollama"
            } else {
                "file"
            },
        });
    }
    for file in &config.files {
        out.push(Candidate {
            path: PathBuf::from(file),
            name: None,
            mmproj: None,
            source: "file",
        });
    }
    for dir in &config.directories {
        let Ok(read) = fs::read_dir(dir) else {
            continue;
        };
        let mut children: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("gguf"))
            .collect();
        children.sort();
        for child in children {
            if excluded.contains(&canonical(&child)) {
                continue;
            }
            out.push(Candidate {
                path: child,
                name: None,
                mmproj: None,
                source: "directory",
            });
        }
    }
    out
}

static KNOWN: LazyLock<Mutex<HashMap<String, GgufEntry>>> = LazyLock::new(Default::default);

/// The last inspected entry for an id, from any catalog scan in this process.
/// Used to resolve picker ids to a model name and context window.
pub fn known(id: &str) -> Option<GgufEntry> {
    KNOWN.lock().ok()?.get(id).cloned()
}

pub fn scan_with(config: &LocalEngineConfig, budget: &Budget) -> Vec<GgufEntry> {
    let mut out: Vec<GgufEntry> = Vec::new();
    let mut seen = HashSet::new();
    for candidate in candidates(config) {
        if out.len() >= MAX_SCAN_FILES {
            break;
        }
        let id = entry_id(&candidate.path);
        if !seen.insert(id) {
            continue;
        }
        match inspect(&candidate, budget) {
            Ok(entry) => out.push(entry),
            // Explicitly added files stay visible with the reason; directory
            // children that are not chat models are simply not models.
            Err(error) if candidate.source != "directory" => {
                out.push(unusable_entry(&candidate, format!("{error:#}")))
            }
            Err(_) => {}
        }
    }
    if let Ok(mut known) = KNOWN.lock() {
        if known.len() > 4096 {
            known.clear();
        }
        for entry in &out {
            known.insert(entry.id.clone(), entry.clone());
        }
    }
    out
}

pub fn scan(config: &LocalEngineConfig) -> Vec<GgufEntry> {
    let runtime = runtime(&config.llama_binary);
    scan_with(config, &Budget::from_runtime(&runtime, config))
}

/// Look up a catalog entry by its stable id only (never by display name).
pub fn entry_for_id(config: &LocalEngineConfig, id: &str) -> Option<GgufEntry> {
    let id = id.trim();
    if !id.starts_with("local:gguf:") {
        return None;
    }
    scan(config).into_iter().find(|entry| entry.id == id)
}

/// Full catalog per the contract. `runtime_state` supplies the loaded server
/// and per-model load errors when an engine is available.
pub fn catalog_with(
    config: &LocalEngineConfig,
    local: Option<&crate::local_runtime::LocalRuntime>,
) -> Value {
    let runtime = runtime(&config.llama_binary);
    let budget = Budget::from_runtime(&runtime, config);
    let mut models = scan_with(config, &budget);
    if let Some(local) = local {
        for entry in &mut models {
            entry.last_error = local.last_error(&entry.id);
        }
    }
    let loaded = local.and_then(|l| l.loaded_json());
    json!({
        "hardware": hardware_json(&runtime),
        "runtime": runtime.to_json(),
        "models": models,
        "loaded": loaded,
        "ollama_store": ollama_store_json(config, &budget),
    })
}

pub fn catalog(config: &LocalEngineConfig) -> Value {
    catalog_with(config, None)
}

/// Composer picker rows for the local group, built from a catalog value.
pub fn picker_rows(catalog: &Value, default_id: &str) -> Vec<Value> {
    let loaded = &catalog["loaded"];
    catalog["models"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|model| {
            let availability = model["availability"].as_str().unwrap_or("unavailable");
            let is_loaded = !loaded.is_null() && loaded["id"] == model["id"];
            let mut reason = model["reason"].as_str().unwrap_or("").to_owned();
            if is_loaded {
                reason = format!(
                    "Loaded · {}",
                    if loaded["fallback_out_of_memory"] == true {
                        "CPU fallback (not enough free GPU memory)".to_owned()
                    } else if loaded["cpu_fallback"] == true {
                        "CPU fallback (GPU load failed)".to_owned()
                    } else {
                        reason
                    }
                );
            } else if let Some(error) = model["last_error"].as_str() {
                reason = format!(
                    "{reason} · Last load failed: {}",
                    crate::tools::truncate(error, 300)
                );
            }
            let vision = if is_loaded {
                loaded["vision"] == true
            } else {
                model["vision"] == true
            };
            json!({
                "id": model["id"],
                "provider": "llamacpp",
                "account": "this-computer",
                "model": model["name"],
                "route": crate::cli_agent::picker::ROUTE_LOCAL,
                "group": crate::cli_agent::picker::GROUP_LOCAL,
                "name": format!("{} · This computer", model["name"].as_str().unwrap_or("GGUF")),
                "subtitle": "Runs on this computer · No subscription quota",
                "inference": "local",
                "availability": availability,
                "availability_label": match availability {
                    "ready" => "Ready",
                    "setup_required" => "Setup required",
                    _ => "Unavailable",
                },
                "reason": reason,
                "featured": true,
                "vision": vision,
                "tools": model["tools"],
                "is_default": model["id"] == default_id,
                "usage": crate::cli_agent::usage::UsageSnapshot::local(),
                "local": model,
            })
        })
        .collect()
}

/// Job-start check for local rows: the id must be in the catalog, and images
/// need a row with vision. Cloud rows are not checked here.
pub fn precheck_job(config: &LocalEngineConfig, model_id: &str, images: usize) -> Result<()> {
    if !model_id.starts_with("local:gguf:") {
        return Ok(());
    }
    let entry = entry_for_id(config, model_id)
        .context("That local model is not in the catalog. Add it in Settings › Local models.")?;
    ensure!(
        images == 0 || entry.vision,
        "{} has no vision projector on this computer, so it cannot read images. Remove the image or pick a model marked Vision.",
        entry.name
    );
    Ok(())
}

fn ollama_store_json(config: &LocalEngineConfig, budget: &Budget) -> Value {
    let Some(root) = crate::ollama_store::discover() else {
        return json!({"path": null, "available": false, "models": []});
    };
    let added: HashSet<PathBuf> = config
        .imports
        .iter()
        .map(|i| canonical(Path::new(&i.path)))
        .chain(config.files.iter().map(|f| canonical(Path::new(f))))
        .collect();
    let models: Vec<Value> = crate::ollama_store::list(&root.path)
        .into_iter()
        .map(|model| {
            let candidate = Candidate {
                path: model.model.clone(),
                name: Some(model.tag.clone()),
                mmproj: Some(model.projector.clone()),
                source: "ollama",
            };
            let (compatible, reason) = match inspect(&candidate, budget) {
                Ok(entry) => (
                    entry.compatible && entry.fits != "no",
                    if entry.compatible && entry.fits != "no" {
                        format!(
                            "{}{}",
                            entry.reason,
                            if entry.vision { " · vision" } else { "" }
                        )
                    } else {
                        entry.reason
                    },
                ),
                Err(error) => (false, format!("{error:#}")),
            };
            let reason = match &model.problem {
                Some(problem) if compatible => format!("{reason}. {problem}"),
                Some(problem) => problem.clone(),
                None => reason,
            };
            json!({
                "tag": model.tag,
                "path": model.model,
                "projector": model.projector,
                "bytes": model.bytes,
                "compatible": compatible && model.problem.as_deref().is_none_or(|p| !p.starts_with("Model blob")),
                "reason": reason,
                "already_added": added.contains(&canonical(&model.model)),
            })
        })
        .collect();
    json!({
        "path": root.path,
        "available": root.path.join("manifests").is_dir(),
        "models": models,
    })
}

/// Register an Ollama store model by reference. Never copies or writes into
/// the store.
pub fn import_ollama(
    config: &LocalEngineConfig,
    root: &Path,
    tag: &str,
) -> Result<LocalEngineConfig> {
    let model = crate::ollama_store::find(root, tag)?;
    if let Some(problem) = &model.problem {
        ensure!(!problem.starts_with("Model blob"), "{problem}");
    }
    let runtime = runtime(&config.llama_binary);
    let budget = Budget::from_runtime(&runtime, config);
    let entry = inspect(
        &Candidate {
            path: model.model.clone(),
            name: Some(model.tag.clone()),
            mmproj: Some(model.projector.clone()),
            source: "ollama",
        },
        &budget,
    )?;
    ensure!(
        entry.compatible,
        "{tag} cannot run on the bundled llama.cpp: {}",
        entry.reason
    );
    let mut next = config.clone();
    let path = model.model.display().to_string();
    next.imports.retain(|i| i.path != path);
    next.imports.push(ImportedModel {
        path,
        mmproj: model.projector.map(|p| p.display().to_string()),
        name: model.tag,
        source: "ollama".into(),
    });
    next.validate()?;
    Ok(next)
}

/// Remove by id or path. Directory-discovered files are excluded instead of
/// silently reappearing. Never deletes weights.
pub fn remove(config: &LocalEngineConfig, key: &str) -> Result<LocalEngineConfig> {
    let key = key.trim();
    ensure!(!key.is_empty(), "Missing catalog id or path");
    let mut next = config.clone();
    let entry = if key.starts_with("local:gguf:") {
        Some(
            scan(config)
                .into_iter()
                .find(|e| e.id == key)
                .context("That local model is not in the catalog")?,
        )
    } else {
        None
    };
    let path = entry
        .as_ref()
        .map(|e| e.path.clone())
        .unwrap_or_else(|| key.to_owned());
    ensure!(
        entry.as_ref().is_none_or(|e| e.source != "download"),
        "ShadowCode downloaded this model. Choose Delete to remove it and free its disk space."
    );
    let before = (next.files.len(), next.directories.len(), next.imports.len());
    next.files.retain(|p| p != &path);
    next.directories.retain(|p| p != &path);
    next.imports.retain(|i| i.path != path);
    let changed = before != (next.files.len(), next.directories.len(), next.imports.len());
    if !changed {
        let from_directory = entry.as_ref().is_some_and(|e| e.source == "directory")
            || config
                .directories
                .iter()
                .any(|d| Path::new(&path).parent().map(canonical) == Some(canonical(Path::new(d))));
        ensure!(
            from_directory,
            "That path is not in the local model catalog"
        );
        if !next.excluded.iter().any(|p| p == &path) {
            next.excluded.push(path);
        }
    }
    next.validate()?;
    Ok(next)
}

/// Add a file or a directory the user chose.
pub fn add(config: &LocalEngineConfig, path: &Path) -> Result<LocalEngineConfig> {
    ensure!(path.is_absolute(), "Choose an absolute file or directory");
    let mut next = config.clone();
    let text = path.display().to_string();
    if path.is_dir() {
        if !next.directories.iter().any(|d| d == &text) {
            next.directories.push(text.clone());
        }
    } else {
        check_addable(path)?;
        if !next.files.iter().any(|d| d == &text) {
            next.files.push(text.clone());
        }
    }
    next.excluded
        .retain(|p| p != &text && !Path::new(p).starts_with(path));
    next.validate()?;
    Ok(next)
}

// ---------------------------------------------------------------------------
// Preparing a model for the engine
// ---------------------------------------------------------------------------

/// A model client ready for one task. For local rows it carries the
/// per-launch bearer key (in memory only) and a lease that keeps the loaded
/// server from being swapped while the task runs.
pub struct PreparedModel {
    pub config: crate::config::ModelConfig,
    pub bearer: Option<String>,
    /// `Some` when the runtime reported the capability (local rows).
    pub vision: Option<bool>,
    pub tools: bool,
    pub extra_body: Option<Value>,
    pub lease: Option<crate::local_runtime::Lease>,
}

impl PreparedModel {
    pub fn passthrough(config: crate::config::ModelConfig) -> Self {
        Self {
            config,
            bearer: None,
            vision: None,
            tools: true,
            extra_body: None,
            lease: None,
        }
    }
    pub fn client(&self, paths: &crate::paths::AppPaths) -> Result<crate::models::ModelClient> {
        Ok(crate::models::ModelClient::new(self.config.clone(), paths)?
            .with_bearer(self.bearer.clone())
            .with_extra_body(self.extra_body.clone()))
    }
    /// Effective vision: runtime-reported for local rows, the provider rule
    /// otherwise.
    pub fn vision(&self) -> bool {
        self.vision.unwrap_or_else(|| {
            crate::vision::model_supports_vision(&self.config.provider, &self.config.name)
        })
    }
    pub fn ensure_images(&self, count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        match self.vision {
            Some(true) => Ok(()),
            Some(false) => bail!(
                "{} runs without a vision projector on this computer, so it cannot read images. Remove the image or pick a model marked Vision.",
                self.config.name
            ),
            None => crate::vision::ensure_vision_or_bail(
                &self.config.provider,
                &self.config.name,
                count,
            ),
        }
    }
    /// Chat-only rows get no tools; `view_image` only for vision rows.
    pub fn filter_schemas(&self, schemas: &mut Vec<Value>) {
        if !self.tools {
            schemas.clear();
            return;
        }
        if self.vision == Some(true) {
            schemas.push(crate::tools::view_image_schema());
        }
    }
}

/// Provider `llamacpp` with a `local:gguf:` id (or no endpoint) is the managed
/// runtime; an explicit endpoint is an external llama.cpp server.
pub fn is_managed(model: &crate::config::ModelConfig) -> bool {
    model.provider == "llamacpp"
        && (model.default.starts_with("local:gguf:") || model.endpoint.trim().is_empty())
}

/// Resolve the catalog entry, start (or reuse) the server, and return a
/// client configuration whose context limit equals the server's `--ctx-size`.
pub async fn prepare(
    config: &LocalEngineConfig,
    model: &crate::config::ModelConfig,
    local: &crate::local_runtime::LocalRuntime,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<PreparedModel> {
    prepare_with_policy(config, model, local, cancel, true).await
}

pub async fn prepare_with_policy(
    config: &LocalEngineConfig,
    model: &crate::config::ModelConfig,
    local: &crate::local_runtime::LocalRuntime,
    cancel: &tokio_util::sync::CancellationToken,
    allow_cpu_fallback: bool,
) -> Result<PreparedModel> {
    prepare_with_progress(
        config,
        model,
        local,
        cancel,
        allow_cpu_fallback,
        &|_| Ok(()),
    )
    .await
}

pub async fn prepare_with_progress(
    config: &LocalEngineConfig,
    model: &crate::config::ModelConfig,
    local: &crate::local_runtime::LocalRuntime,
    cancel: &tokio_util::sync::CancellationToken,
    allow_cpu_fallback: bool,
    progress: &(dyn Fn(crate::local_runtime::Progress) -> Result<()> + Send + Sync),
) -> Result<PreparedModel> {
    let id = model.default.trim().to_owned();
    ensure!(!cancel.is_cancelled(), "Model preparation cancelled");
    let config_owned = config.clone();
    let scan = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut runtime = runtime(&config_owned.llama_binary);
        let budget = Budget::from_runtime(&runtime, &config_owned);
        let candidate = candidates(&config_owned)
            .into_iter()
            .find(|c| entry_id(&c.path) == id)
            .context("That local model is not in the catalog. Add it in Settings › Local models (removing a row never deletes weights).")?;
        ensure!(runtime.ready(), "{}", runtime.detail);
        // Resolve the projector, then freeze the files before the final
        // compatibility/context/template inspection used by this task.
        let initial = inspect(&candidate, &budget)?;
        let projector = initial.mmproj.as_ref().map(PathBuf::from);
        let sources = crate::local_runtime::SourceIdentity {
            model: FileKey::capture(&candidate.path)?,
            runtime: FileKey::capture(runtime.path.as_deref().context("Runtime path missing")?)?,
            projector: projector.as_deref().map(FileKey::capture).transpose()?,
        };
        runtime.probe = Some(probe(
            runtime.path.as_deref().context("Runtime path missing")?,
        ));
        ensure!(
            runtime.probe.as_ref().is_some_and(|p| p.ok),
            "Runtime changed or became unavailable during preparation"
        );
        let budget = Budget::from_runtime(&runtime, &config_owned);
        let entry = inspect(
            &Candidate {
                mmproj: Some(projector),
                ..candidate
            },
            &budget,
        )?;
        let template_profile =
            crate::local_templates::select(header(Path::new(&entry.path))?.as_ref());
        Ok((runtime, entry, sources, template_profile))
    });
    let (runtime, entry, sources, template_profile) = tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("Model preparation cancelled"),
        result = scan => result.context("Local catalog scan stopped")??,
    };
    ensure!(runtime.ready(), "{}", runtime.detail);
    ensure!(
        entry.compatible,
        "{} cannot run on this computer: {}",
        entry.name,
        entry.reason
    );
    ensure!(entry.fits != "no", "{}: {}", entry.name, entry.reason);
    let ctx = if model.context_limit == 0 {
        entry.context_tokens
    } else {
        model.context_limit as u64
    };
    ensure!(
        ctx <= entry.context_tokens,
        "{} can use at most {} tokens of context on this computer, but this task was prepared for {}. Pick the model again in the composer.",
        entry.name,
        entry.context_tokens,
        ctx
    );
    let binary = runtime.path.clone().context("Runtime path missing")?;
    let gpu = if runtime.gpu().is_none() {
        crate::local_runtime::GpuMode::Off
    } else if entry.fits == "gpu" {
        crate::local_runtime::GpuMode::All
    } else {
        crate::local_runtime::GpuMode::Auto
    };
    let spec = crate::local_runtime::LaunchSpec {
        id: entry.id.clone(),
        name: entry.name.clone(),
        binary,
        model: PathBuf::from(&entry.path),
        mmproj: entry.mmproj.as_ref().map(PathBuf::from),
        ctx,
        gpu,
        backend: runtime.backend(),
        template_profile,
    };
    let (loaded, lease) = local
        .acquire_checked_with_progress(spec, cancel, allow_cpu_fallback, progress, Some(sources))
        .await?;
    let mut next = model.clone();
    next.endpoint = loaded.endpoint.clone();
    next.name = entry.name.clone();
    next.context_limit = loaded.ctx as usize;
    next.api_key_env = "UNUSED".into();
    Ok(PreparedModel {
        config: next,
        bearer: Some(loaded.api_key.clone()),
        vision: Some(loaded.vision),
        tools: entry.tools,
        extra_body: entry
            .thinking_switch
            .then(|| json!({"chat_template_kwargs": {"enable_thinking": false}})),
        lease: Some(lease),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gguf::test_support::{write_gguf, V};

    #[test]
    fn header_cache_detects_replacement_with_identical_size_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.gguf");
        model(&path, "qwen3", Some("old_template"));
        let before = header(&path).unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let replacement = path.with_extension("new");
        model(&replacement, "qwen3", Some("new_template"));
        fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
            .unwrap();
        fs::rename(&replacement, &path).unwrap();
        let after = header(&path).unwrap();
        assert_eq!(metadata.len(), fs::metadata(&path).unwrap().len());
        assert_eq!(after.chat_template(), Some("new_template"));
        assert_ne!(before.header_sha256, after.header_sha256);
    }

    fn model(path: &Path, arch: &str, template: Option<&str>) {
        // No general.name: rows fall back to the file name.
        let mut kv = vec![("general.architecture", V::Str(arch))];
        let ctx_key = format!("{arch}.context_length");
        let emb_key = format!("{arch}.embedding_length");
        let blk_key = format!("{arch}.block_count");
        kv.push((ctx_key.as_str(), V::U32(32768)));
        kv.push((emb_key.as_str(), V::U32(2048)));
        kv.push((blk_key.as_str(), V::U32(16)));
        if let Some(t) = template {
            kv.push(("tokenizer.chat_template", V::Str(t)));
        }
        write_gguf(path, &kv, &["token_embd.weight", "output.weight"]);
    }

    #[test]
    fn file_models_are_named_from_gguf_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let named = |path: &Path| {
            write_gguf(
                path,
                &[
                    ("general.architecture", V::Str("qwen3")),
                    ("general.name", V::Str("  Qwen3   14B ")),
                ],
                &["token_embd.weight"],
            )
        };
        let path = dir.path().join("qwen3-14b-instruct-Q4_K_M.gguf");
        named(&path);
        let header = header(&path).unwrap();
        assert_eq!(display_name(&header, &path), "Qwen3 14B · Q4_K_M");
        let plain = dir.path().join("weights.gguf");
        named(&plain);
        assert_eq!(
            display_name(&super::header(&plain).unwrap(), &plain),
            "Qwen3 14B"
        );
        let unnamed = dir.path().join("mystery-BF16.gguf");
        write_gguf(
            &unnamed,
            &[("general.architecture", V::Str("llama"))],
            &["token_embd.weight"],
        );
        let bare = super::header(&unnamed).unwrap();
        assert_eq!(display_name(&bare, &unnamed), "mystery-BF16");
    }

    fn projector(path: &Path, width: u32) {
        write_gguf(
            path,
            &[
                ("general.architecture", V::Str("clip")),
                ("clip.has_vision_encoder", V::Bool(true)),
                ("clip.vision.projection_dim", V::U32(width)),
            ],
            &["v.patch_embd.weight"],
        );
    }

    fn budget(archs: &[&str]) -> Budget {
        Budget {
            runtime_ready: true,
            gpu_bytes: Some(16 * GIB),
            ram_bytes: 64 * GIB,
            context_cap: DEFAULT_CONTEXT_CAP,
            architectures: Some(Arc::new(archs.iter().map(|s| s.to_string()).collect())),
        }
    }

    #[test]
    fn template_words_are_only_hints_and_preserve_schema_policy() {
        let dir = tempfile::tempdir().unwrap();
        for (name, template, expected_tools, expected_basis) in [
            (
                "comment",
                Some("{# tools are unavailable #}{{ messages }}"),
                true,
                "template_hint",
            ),
            (
                "literal",
                Some("No tool_call protocol. {{ messages }}"),
                true,
                "template_hint",
            ),
            ("chat", Some("{{ messages }}"), false, "no_template_hint"),
            ("missing", None, false, "no_template"),
        ] {
            let path = dir.path().join(format!("{name}.gguf"));
            model(&path, "qwen3", template);
            let config = LocalEngineConfig {
                files: vec![path.display().to_string()],
                ..Default::default()
            };
            let entries = scan_with(&config, &budget(&["qwen3"]));
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].tools, expected_tools);
            assert_eq!(json!(&entries[0])["tools_basis"], expected_basis);
            if expected_tools {
                assert!(entries[0].tools_reason.contains("file hint only"));
                assert!(!entries[0].tools_reason.contains("supports tool calls"));
            }
            // Old catalog snapshots remain readable without gaining a claim.
            let mut old = json!(&entries[0]);
            old.as_object_mut().unwrap().remove("tools_basis");
            let old: GgufEntry = serde_json::from_value(old).unwrap();
            assert_eq!(json!(old)["tools_basis"], "unknown");
        }
    }

    #[test]
    fn tool_hint_does_not_infer_absence_beyond_retained_template_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.gguf");
        let template = format!("{}{{{{ tools }}}}", "x".repeat(64 * 1024));
        model(&path, "qwen3", Some(&template));
        let config = LocalEngineConfig {
            files: vec![path.display().to_string()],
            ..Default::default()
        };
        let entries = scan_with(&config, &budget(&["qwen3"]));
        assert!(!entries[0].tools);
        assert_eq!(json!(&entries[0])["tools_basis"], "no_template_hint");
        assert!(entries[0].tools_reason.contains("retained template"));
        assert!(!entries[0].tools_reason.contains("no tool-call support"));
        assert_eq!(
            header(&path).unwrap().template_identity,
            Some(gguf::string_identity(&template))
        );
    }

    #[test]
    fn metadata_decides_models_projectors_tools_and_compatibility() {
        let dir = tempfile::tempdir().unwrap();
        let qwen = dir.path().join("qwen.gguf");
        model(&qwen, "qwen3", Some("{% if tools %}<tool_call>{% endif %}"));
        let chat = dir.path().join("chat-model-with-tools-in-name.gguf");
        model(&chat, "llama", Some("{{ messages }}"));
        let gptoss = dir.path().join("oss.gguf");
        model(&gptoss, "gptoss", Some("tools"));
        projector(&dir.path().join("qwen.mmproj.gguf"), 2048);
        let vocab = dir.path().join("ggml-vocab-llama.gguf");
        write_gguf(&vocab, &[("general.architecture", V::Str("llama"))], &[]);
        fs::write(dir.path().join("notes.gguf"), b"not gguf").unwrap();
        let config = LocalEngineConfig {
            directories: vec![dir.path().display().to_string()],
            ..Default::default()
        };
        let entries = scan_with(&config, &budget(&["qwen3", "llama", "gpt-oss"]));
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names.len(), 3, "{names:?}");
        let qwen = entries.iter().find(|e| e.name == "qwen").unwrap();
        assert!(qwen.compatible && qwen.tools && qwen.vision);
        assert!(qwen
            .mmproj
            .as_deref()
            .unwrap()
            .ends_with("qwen.mmproj.gguf"));
        assert_eq!(qwen.context_train, Some(32768));
        assert_eq!(qwen.context_tokens, 16384);
        assert_eq!(qwen.fits, "gpu");
        assert_eq!(qwen.availability, "ready");
        assert!(qwen.id.starts_with("local:gguf:"));
        let chat = entries
            .iter()
            .find(|e| e.name.starts_with("chat-model"))
            .unwrap();
        assert!(!chat.tools, "tools come from the template, not the name");
        assert!(chat.tools_reason.contains("Chat only"));
        assert!(
            !chat.vision,
            "a folder projector pairs only when the folder holds one model"
        );
        let oss = entries.iter().find(|e| e.name == "oss").unwrap();
        assert!(!oss.compatible);
        assert_eq!(oss.reason, "unsupported architecture gptoss");
        assert_eq!(oss.availability, "unavailable");
        // Adding a projector or vocab file directly is refused with a reason.
        assert!(check_addable(&dir.path().join("qwen.mmproj.gguf"))
            .unwrap_err()
            .to_string()
            .contains("projector"));
        assert!(check_addable(&vocab)
            .unwrap_err()
            .to_string()
            .contains("vocabulary"));
        // Lookup is by id only.
        assert!(entry_for_id(&config, "qwen").is_none());
        assert!(known(&qwen.id).is_some());
    }

    #[test]
    fn single_projector_in_folder_pairs_only_when_widths_match() {
        let dir = tempfile::tempdir().unwrap();
        let m = dir.path().join("gemma.gguf");
        model(&m, "gemma4", Some("tools"));
        projector(&dir.path().join("mmproj-bf16.gguf"), 2048);
        let entry = inspect(
            &Candidate {
                path: m.clone(),
                name: None,
                mmproj: None,
                source: "file",
            },
            &budget(&["gemma4"]),
        )
        .unwrap();
        assert!(entry.vision);
        projector(&dir.path().join("mmproj-bf16.gguf"), 4096);
        let entry = inspect(
            &Candidate {
                path: m,
                name: None,
                mmproj: None,
                source: "file",
            },
            &budget(&["gemma4"]),
        )
        .unwrap();
        assert!(!entry.vision, "mismatched projector must not pair");
    }

    #[test]
    fn memory_planning_shrinks_context_then_falls_back_to_cpu_then_no() {
        let dir = tempfile::tempdir().unwrap();
        let m = dir.path().join("m.gguf");
        model(&m, "llama", Some("tools"));
        let header = header(&m).unwrap();
        let mut b = budget(&["llama"]);
        let (ctx, est, fits) = plan_context(&header, 4 * GIB, 0, &b);
        assert_eq!((ctx, fits), (16384, "gpu"));
        assert!(est.total_bytes > 4 * GIB);
        let (_, _, fits) = plan_context(&header, 20 * GIB, 0, &b);
        assert_eq!(fits, "cpu");
        b.ram_bytes = 8 * GIB;
        b.gpu_bytes = None;
        let (ctx, _, fits) = plan_context(&header, 20 * GIB, 0, &b);
        assert_eq!((ctx, fits), (4096, "no"));
    }

    #[test]
    fn parses_list_devices_and_version() {
        let text = "0.00.000.439 I srv  llama_server: initializing ...\nAvailable devices:\n  Vulkan0: NVIDIA GeForce RTX 5060 Ti (16557 MiB, 14807 MiB free)\n";
        let devices = parse_devices(text);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "Vulkan0");
        assert_eq!(devices[0].name, "NVIDIA GeForce RTX 5060 Ti");
        assert_eq!(devices[0].total_bytes, 16557 * 1024 * 1024);
        assert_eq!(backend_of(&devices), "vulkan");
        assert!(parse_devices("Available devices:\n").is_empty());
        assert_eq!(backend_of(&[]), "cpu");
        let (v, c) =
            parse_version("x\nversion: 0.4.1-dev (build 1, commit 18f9f7bef)\nbuilt with GNU")
                .unwrap();
        assert_eq!(v, "0.4.1-dev");
        assert_eq!(c.as_deref(), Some("18f9f7bef"));
    }

    #[test]
    fn resolution_prefers_newer_bundle_and_never_llama_cli_or_path() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("bundle");
        let managed = root.path().join("managed");
        fs::create_dir_all(&bundle).unwrap();
        fs::create_dir_all(&managed).unwrap();
        fs::write(bundle.join(MANAGED_SERVER), b"b").unwrap();
        fs::write(managed.join(MANAGED_SERVER), b"m").unwrap();
        fs::write(bundle.join("COMMIT"), "built=2026-09-23T12:08:43Z\n").unwrap();
        fs::write(managed.join("COMMIT"), "built=2026-09-23T10:58:29Z\n").unwrap();
        let env = root.path().join("env-llama-server");
        fs::write(&env, b"e").unwrap();
        let order = runtime_candidates_with(
            "",
            Some(bundle.clone()),
            Some(managed.clone()),
            Some(env.clone()),
        );
        let origins: Vec<_> = order.iter().map(|(_, o)| *o).collect();
        // An explicit SHADOWCODE_LLAMA_SERVER override wins over installed runtimes.
        assert_eq!(origins, ["other", "bundled", "managed"]);
        fs::write(managed.join("COMMIT"), "built=2026-09-24T00:00:00Z\n").unwrap();
        let order = runtime_candidates_with("", Some(bundle.clone()), Some(managed.clone()), None);
        assert_eq!(order[0].1, "managed");
        // llama-cli is never a runtime, even when configured.
        let cli = root.path().join("llama-cli");
        fs::write(&cli, b"c").unwrap();
        let order = runtime_candidates_with(&cli.display().to_string(), None, None, None);
        assert!(order.is_empty());
        // Nothing found means Setup required, not a PATH guess.
        let runtime = runtime_from(Vec::new());
        assert_eq!(runtime.state, "setup_required");
    }

    #[test]
    fn remove_directory_entry_excludes_it_and_add_refuses_projector() {
        let dir = tempfile::tempdir().unwrap();
        let m = dir.path().join("a.gguf");
        model(&m, "llama", Some("tools"));
        let config = LocalEngineConfig {
            directories: vec![dir.path().display().to_string()],
            ..Default::default()
        };
        let next = remove(&config, &m.display().to_string()).unwrap();
        assert_eq!(next.excluded, vec![m.display().to_string()]);
        assert!(scan_with(&next, &budget(&["llama"])).is_empty());
        assert!(m.exists(), "weights are never deleted");
        let readded = add(&next, &m).unwrap();
        assert!(readded.excluded.is_empty());
        assert!(remove(&config, "/nowhere/x.gguf").is_err());
    }
}

//! Chat models ShadowCode can download for people without an account.
//!
//! Nothing downloads on its own: a model is fetched only when the user
//! chooses Download (on first run or in Settings › Local models). Every
//! entry is pinned to one Hugging Face commit, and the file must match its
//! recorded size and SHA-256 before it appears under its final name.
//! Downloads resume with HTTP range requests, can be paused or cancelled,
//! check the free disk space first, and are refused in offline mode (by the
//! route). One model downloads at a time.
//!
//! Finished files live in `<data>/local-models/` and join the local catalog
//! by themselves ([`installed`], used by `local_engine::candidates`) under
//! the catalog's name. Deleting one removes the file.
use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

/// Folder under the data directory that holds downloaded models.
pub const DIR: &str = "local-models";
const PART_SUFFIX: &str = ".part";
const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
/// Space left on the disk after a download, for the conversation store and
/// logs.
const DISK_HEADROOM: u64 = 512 * MIB;
/// No data for this long ends the attempt; the partial file is kept.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// VRAM left free (matches the local engine's planner).
const VRAM_MARGIN: u64 = GIB;
/// RAM left to the desktop, browser and editor when the recommendation
/// runs a model on the processor.
const CPU_COMFORT: u64 = 3 * GIB;
/// RAM the local engine itself leaves free when it plans a CPU load.
const RAM_MARGIN: u64 = 2 * GIB;

/// One downloadable model. `memory_bytes` and `min_memory_bytes` are
/// ShadowCode's own estimate (`gguf::estimate_memory` on the file's header)
/// at the default 16,384-token context and at the 4,096-token minimum.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct CatalogModel {
    pub id: &'static str,
    pub name: &'static str,
    pub publisher: &'static str,
    /// Hugging Face repository that publishes the GGUF.
    pub repo: &'static str,
    /// The repository commit the URL is pinned to.
    pub commit: &'static str,
    pub file: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    /// SPDX identifier, checked against the original model's repository.
    pub license: &'static str,
    pub license_url: &'static str,
    /// `general.architecture` in the file; must be in the runtime's
    /// `architectures.txt`.
    pub architecture: &'static str,
    pub quantization: &'static str,
    pub memory_bytes: u64,
    pub min_memory_bytes: u64,
    /// Few parameters are active per token (small or mixture-of-experts),
    /// so it answers at a usable speed on a processor without a GPU.
    pub cpu_friendly: bool,
    pub summary: &'static str,
}

impl CatalogModel {
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.commit, self.file
        )
    }
    /// The repository page at the pinned commit (model card and license).
    pub fn source_url(&self) -> String {
        format!("https://huggingface.co/{}/tree/{}", self.repo, self.commit)
    }
    pub fn spec(&self) -> Spec {
        Spec {
            id: self.id.into(),
            url: self.url(),
            file: self.file.into(),
            sha256: self.sha256.into(),
            bytes: self.bytes,
        }
    }
}

/// Smallest to largest; later entries are stronger. Verified 2026-09-28:
/// sizes and SHA-256 from each repository's LFS pointers at the commit,
/// licenses from the original model repositories, memory from the GGUF
/// headers (`catalog_matches_hugging_face` re-checks all of it).
pub const CATALOG: &[CatalogModel] = &[
    CatalogModel {
        id: "granite-4.2-3b",
        name: "Granite 4.2 3B",
        publisher: "IBM",
        repo: "ibm-granite/granite-4.2-3b-GGUF",
        commit: "c40945d71cd90f249a56985e8155551a9188dc30",
        file: "granite-4.2-3b-Q4_K_M.gguf",
        sha256: "e0406663965846ae22a403456eb826ccce5f450840491f71952f18a7cb78e7d5",
        bytes: 2_244_011_552,
        license: "Apache-2.0",
        license_url: "https://huggingface.co/ibm-granite/granite-4.2-3b",
        architecture: "granite",
        quantization: "Q4_K_M",
        memory_bytes: 4_391_495_200,
        min_memory_bytes: 3_384_862_240,
        cpu_friendly: true,
        summary: "Small and quick. Runs on most computers, even without a graphics card.",
    },
    CatalogModel {
        id: "gemma-4-e4b",
        name: "Gemma 4 E4B",
        publisher: "Google",
        repo: "google/gemma-4-E4B-it-qat-q4_0-gguf",
        commit: "4b4a2c1d584be7264f87aac328a1bc739ce81b6c",
        file: "gemma-4-E4B_q4_0-it.gguf",
        sha256: "676c35070db6dbe52f93e9c864ee0fba4eddea94b9c875d9cb10daff453fbaee",
        bytes: 5_154_941_280,
        license: "Apache-2.0",
        license_url: "https://ai.google.dev/gemma/docs/gemma_4_license",
        architecture: "gemma4",
        quantization: "Q4_0 (QAT)",
        memory_bytes: 6_503_410_016,
        min_memory_bytes: 6_151_088_480,
        cpu_friendly: true,
        summary: "Balanced. Good answers on a laptop with 16 GB of memory.",
    },
    CatalogModel {
        id: "gemma-4-12b",
        name: "Gemma 4 12B",
        publisher: "Google",
        repo: "google/gemma-4-12B-it-qat-q4_0-gguf",
        commit: "29d097773436b69ff9feafd636ab4cf873786537",
        file: "gemma-4-12b-it-qat-q4_0.gguf",
        sha256: "93567e57a8fe10b23569b9d9ec38cd005deedf71e29477c421a4b83f418a538b",
        bytes: 6_975_879_296,
        license: "Apache-2.0",
        license_url: "https://ai.google.dev/gemma/docs/gemma_4_license",
        architecture: "gemma4",
        quantization: "Q4_0 (QAT)",
        memory_bytes: 8_552_937_600,
        min_memory_bytes: 8_351_611_008,
        cpu_friendly: false,
        summary: "Stronger. Best with a graphics card that has 10 GB or more.",
    },
    CatalogModel {
        id: "gpt-oss-20b",
        name: "gpt-oss 20B",
        publisher: "OpenAI",
        repo: "ggml-org/gpt-oss-20b-GGUF",
        commit: "ef9b12f2ff56c69cf32153a02784e7a3c88bf524",
        file: "gpt-oss-20b-MXFP4.gguf",
        sha256: "27cd6c432c7672cb812a92f611cf3ba7bbc35928262bb1e1253ff4ee6ae35901",
        bytes: 12_109_566_624,
        license: "Apache-2.0",
        license_url: "https://huggingface.co/openai/gpt-oss-20b",
        architecture: "gpt-oss",
        quantization: "MXFP4",
        memory_bytes: 13_720_179_360,
        min_memory_bytes: 13_116_199_584,
        cpu_friendly: true,
        summary: "Strong reasoning and tool use. Wants a 16 GB graphics card or 24 GB of memory.",
    },
    CatalogModel {
        id: "qwen3.6-35b-a3b",
        name: "Qwen3.6 35B-A3B",
        publisher: "Qwen",
        repo: "ggml-org/Qwen3.6-35B-A3B-GGUF",
        commit: "baec3ebee244827cda0f4557eafa8b28f7545fa6",
        file: "Qwen3.6-35B-A3B-Q4_K_M.gguf",
        sha256: "671e47e0ec53c665d048b98c3ecbfd5236b5ca9c3e02ed19fc8f81f7b85140c7",
        bytes: 20_419_565_568,
        license: "Apache-2.0",
        license_url: "https://huggingface.co/Qwen/Qwen3.6-35B-A3B",
        architecture: "qwen35moe",
        quantization: "Q4_K_M",
        memory_bytes: 22_567_049_216,
        min_memory_bytes: 21_560_416_256,
        cpu_friendly: true,
        summary: "The strongest coder here. Needs 32 GB of memory or a 24 GB graphics card.",
    },
];

pub fn entry(id: &str) -> Option<&'static CatalogModel> {
    CATALOG.iter().find(|m| m.id == id)
}

pub fn dir(paths: &crate::paths::AppPaths) -> PathBuf {
    paths.data.join(DIR)
}

pub fn final_path(dir: &Path, file: &str) -> PathBuf {
    dir.join(file)
}

fn part_path(dir: &Path, file: &str) -> PathBuf {
    dir.join(format!("{file}{PART_SUFFIX}"))
}

/// Catalog models whose file is complete in `dir` (exact size; the hash was
/// checked before the file got its name).
pub fn installed(dir: &Path) -> Vec<(&'static CatalogModel, PathBuf)> {
    CATALOG
        .iter()
        .filter_map(|model| {
            let path = final_path(dir, model.file);
            fs::metadata(&path)
                .is_ok_and(|m| m.is_file() && m.len() == model.bytes)
                .then_some((model, path))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Hardware fit and the recommendation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Fully in GPU memory at the default context.
    Gpu,
    /// On the processor, with room left for other apps.
    Cpu,
    /// Only with a shorter context, and other apps may slow down.
    Tight,
    /// Not enough memory.
    No,
}

pub fn fit(model: &CatalogModel, ram_bytes: u64, vram_bytes: Option<u64>) -> Fit {
    if vram_bytes.is_some_and(|v| v.saturating_sub(VRAM_MARGIN) >= model.memory_bytes) {
        Fit::Gpu
    } else if ram_bytes.saturating_sub(CPU_COMFORT) >= model.memory_bytes {
        Fit::Cpu
    } else if ram_bytes.saturating_sub(RAM_MARGIN) >= model.min_memory_bytes {
        Fit::Tight
    } else {
        Fit::No
    }
}

/// The strongest model that runs on the GPU, else the strongest one that
/// runs well on the processor, else the smallest one that fits at all.
pub fn recommend(ram_bytes: u64, vram_bytes: Option<u64>) -> Option<(&'static CatalogModel, Fit)> {
    recommend_among(ram_bytes, vram_bytes, |_| true)
}

/// [`recommend`] limited to the models `usable` accepts (the architectures
/// the runtime on this computer can load).
pub fn recommend_among(
    ram_bytes: u64,
    vram_bytes: Option<u64>,
    usable: impl Fn(&CatalogModel) -> bool,
) -> Option<(&'static CatalogModel, Fit)> {
    let fits = |model: &CatalogModel| fit(model, ram_bytes, vram_bytes);
    let usable: Vec<&'static CatalogModel> = CATALOG.iter().filter(|m| usable(m)).collect();
    usable
        .iter()
        .rev()
        .find(|m| fits(m) == Fit::Gpu)
        .or_else(|| {
            usable
                .iter()
                .rev()
                .find(|m| m.cpu_friendly && fits(m) == Fit::Cpu)
        })
        .or_else(|| usable.iter().find(|m| fits(m) == Fit::Tight))
        .map(|m| (*m, fits(m)))
}

// ---------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------

/// What to fetch. Built from a [`CatalogModel`]; tests point it at a local
/// server.
#[derive(Clone, Debug)]
pub struct Spec {
    pub id: String,
    pub url: String,
    pub file: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Not on this computer.
    Available,
    Downloading,
    /// Reading the part already on disk before resuming.
    Checking,
    /// Stopped with a partial file that Resume continues.
    Paused,
    /// The last attempt failed; `done` > 0 means Resume can continue.
    Failed,
    Installed,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub state: State,
    pub done: u64,
    pub total: u64,
    pub bytes_per_second: u64,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Pause,
    Cancel,
}

struct Slot {
    status: Status,
    cancel: CancellationToken,
    stop: Option<Stop>,
    /// Speed window: when and at how many bytes it started.
    window: (Instant, u64),
}

impl Slot {
    fn active(&self) -> bool {
        matches!(self.status.state, State::Downloading | State::Checking)
    }
}

type SpaceProbe = fn(&Path) -> std::io::Result<u64>;

/// Asked every few seconds while a download runs; an error stops it and
/// keeps the partial file (the route stops downloads when offline mode is
/// turned on).
pub type Guard = Arc<dyn Fn() -> std::result::Result<(), String> + Send + Sync>;

/// Download bookkeeping, keyed by the final path so two profiles never
/// share a slot.
pub struct Downloader {
    slots: Arc<Mutex<HashMap<PathBuf, Slot>>>,
    available_space: SpaceProbe,
    /// How often a running download asks its guard.
    guard_every: Duration,
}

impl Default for Downloader {
    fn default() -> Self {
        Self::with_space_probe(|path| fs2::available_space(path))
    }
}

static DOWNLOADER: LazyLock<Downloader> = LazyLock::new(Downloader::default);

/// The downloader the application routes use.
pub fn downloader() -> &'static Downloader {
    &DOWNLOADER
}

fn partial_len(dir: &Path, file: &str) -> u64 {
    fs::metadata(part_path(dir, file))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Sizes as the window shows them (binary units, "4.8 GB").
fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / GIB as f64)
}

impl Downloader {
    pub fn with_space_probe(available_space: SpaceProbe) -> Self {
        Self {
            slots: Arc::default(),
            available_space,
            guard_every: Duration::from_secs(2),
        }
    }

    /// The model's state from the running download, the last failure, or
    /// the files on disk.
    pub fn status(&self, spec: &Spec, dir: &Path) -> Status {
        let target = final_path(dir, &spec.file);
        let on_disk = |state, done| Status {
            state,
            done,
            total: spec.bytes,
            bytes_per_second: 0,
            error: None,
        };
        if let Some(slot) = self.slots.lock().ok().and_then(|s| {
            s.get(&target)
                .filter(|slot| slot.active() || slot.status.state == State::Failed)
                .map(|slot| slot.status.clone())
        }) {
            if slot.state == State::Failed {
                return Status {
                    done: partial_len(dir, &spec.file),
                    ..slot
                };
            }
            return slot;
        }
        if fs::metadata(&target).is_ok_and(|m| m.len() == spec.bytes) {
            return on_disk(State::Installed, spec.bytes);
        }
        match partial_len(dir, &spec.file) {
            0 => on_disk(State::Available, 0),
            done => on_disk(State::Paused, done.min(spec.bytes)),
        }
    }

    /// True while any download or resume check runs.
    pub fn busy(&self) -> bool {
        self.slots
            .lock()
            .is_ok_and(|slots| slots.values().any(Slot::active))
    }

    /// Free space where `dir` is (or would be created).
    fn free_space(&self, dir: &Path) -> Option<u64> {
        let existing = dir.ancestors().find(|p| p.is_dir())?;
        (self.available_space)(existing).ok()
    }

    /// Check what can be checked right away (installed, one at a time, free
    /// space), then download in the background. Resumes a partial file.
    pub fn start(&self, spec: Spec, dir: PathBuf, client: reqwest::Client) -> Result<()> {
        self.start_guarded(spec, dir, client, Arc::new(|| Ok(())))
    }

    /// [`Downloader::start`] that stops (keeping the partial file) as soon
    /// as `guard` refuses.
    pub fn start_guarded(
        &self,
        spec: Spec,
        dir: PathBuf,
        client: reqwest::Client,
        guard: Guard,
    ) -> Result<()> {
        let target = final_path(&dir, &spec.file);
        ensure!(
            !fs::metadata(&target).is_ok_and(|m| m.len() == spec.bytes),
            "This model is already downloaded"
        );
        crate::paths::private_directory(&dir)?;
        let have = partial_len(&dir, &spec.file).min(spec.bytes);
        let needed = spec.bytes - have;
        let free = (self.available_space)(&dir)
            .with_context(|| format!("Could not check the free space in {}", dir.display()))?;
        ensure!(
            free >= needed.saturating_add(DISK_HEADROOM),
            "Not enough disk space: this download needs {} more and {} is free in {}. Free up space and try again.",
            gb(needed.saturating_add(DISK_HEADROOM)),
            gb(free),
            dir.display()
        );
        let cancel = CancellationToken::new();
        {
            let mut slots = self
                .slots
                .lock()
                .map_err(|_| anyhow::anyhow!("Download state is unavailable"))?;
            ensure!(
                !slots.values().any(Slot::active),
                "Another model is downloading. Pause it or wait until it finishes."
            );
            slots.insert(
                target.clone(),
                Slot {
                    status: Status {
                        state: if have > 0 {
                            State::Checking
                        } else {
                            State::Downloading
                        },
                        done: have,
                        total: spec.bytes,
                        bytes_per_second: 0,
                        error: None,
                    },
                    cancel: cancel.clone(),
                    stop: None,
                    window: (Instant::now(), have),
                },
            );
        }
        let slots = self.slots.clone();
        let guard_every = self.guard_every;
        tokio::spawn(async move {
            let key = target.clone();
            let report_slots = slots.clone();
            let report = move |state: State, done: u64| {
                if let Ok(mut map) = report_slots.lock() {
                    if let Some(slot) = map.get_mut(&key) {
                        if slot.stop.is_some() {
                            return;
                        }
                        let elapsed = slot.window.0.elapsed();
                        if state != slot.status.state {
                            slot.window = (Instant::now(), done);
                            slot.status.bytes_per_second = 0;
                        } else if elapsed >= Duration::from_secs(1) {
                            let rate = (done.saturating_sub(slot.window.1) as f64
                                / elapsed.as_secs_f64())
                                as u64;
                            // Smooth so the time left doesn't jump around.
                            slot.status.bytes_per_second = if slot.status.bytes_per_second == 0 {
                                rate
                            } else {
                                (slot.status.bytes_per_second * 2 + rate) / 3
                            };
                            slot.window = (Instant::now(), done);
                        }
                        slot.status.state = state;
                        slot.status.done = done;
                    }
                }
            };
            let checks = Checks {
                cancel: &cancel,
                guard: &guard,
                every: guard_every,
            };
            let result = fetch(&spec, &dir, &client, checks, report).await;
            let Ok(mut map) = slots.lock() else {
                return;
            };
            let stop = map.get(&target).and_then(|slot| slot.stop);
            match (result, stop) {
                (_, Some(Stop::Cancel)) => {
                    let _ = fs::remove_file(part_path(&dir, &spec.file));
                    map.remove(&target);
                }
                (_, Some(Stop::Pause)) | (Ok(()), None) => {
                    map.remove(&target);
                }
                (Err(error), None) => {
                    if let Some(slot) = map.get_mut(&target) {
                        slot.status.state = State::Failed;
                        slot.status.bytes_per_second = 0;
                        slot.status.error = Some(format!("{error:#}"));
                    }
                }
            }
        });
        Ok(())
    }

    fn stop(&self, dir: &Path, file: &str, how: Stop) -> bool {
        let target = final_path(dir, file);
        let Ok(mut slots) = self.slots.lock() else {
            return false;
        };
        match slots.get_mut(&target) {
            Some(slot) if slot.active() => {
                slot.stop = Some(how);
                slot.cancel.cancel();
                true
            }
            Some(_) => {
                // A failed attempt: forget it (Cancel also deletes the part).
                slots.remove(&target);
                false
            }
            None => false,
        }
    }

    /// Stop and keep the partial file for Resume. False when nothing ran.
    pub fn pause(&self, dir: &Path, file: &str) -> bool {
        self.stop(dir, file, Stop::Pause)
    }

    /// Stop and delete the partial file. Returns whether anything was
    /// running or left on disk.
    pub fn cancel(&self, dir: &Path, file: &str) -> Result<bool> {
        let running = self.stop(dir, file, Stop::Cancel);
        let part = part_path(dir, file);
        let existed = part.exists();
        if existed && !running {
            fs::remove_file(&part)
                .with_context(|| format!("Could not delete {}", part.display()))?;
        }
        Ok(running || existed)
    }

    /// Delete a downloaded model and any partial file. The caller unloads it
    /// first.
    pub fn delete(&self, dir: &Path, file: &str) -> Result<bool> {
        self.cancel(dir, file)?;
        let target = final_path(dir, file);
        let existed = target.exists();
        if existed {
            fs::remove_file(&target)
                .with_context(|| format!("Could not delete {}", target.display()))?;
        }
        Ok(existed)
    }

    /// Wait until no download runs (tests).
    #[doc(hidden)]
    pub async fn idle(&self) {
        while self
            .slots
            .lock()
            .map(|s| s.values().any(Slot::active))
            .unwrap_or(false)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Let the task store its final state.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// HTTP client for model downloads: follows the Hugging Face redirect to its
/// CDN and gives up on a connection that can't be made in 20 seconds.
pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .user_agent(concat!("ShadowCode/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

fn plain_io(error: std::io::Error, what: &str) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::StorageFull || error.raw_os_error() == Some(libc::ENOSPC)
    {
        anyhow::anyhow!(
            "The disk is full. Free up space, then choose Resume to continue where it stopped."
        )
    } else {
        anyhow::Error::new(error).context(what.to_owned())
    }
}

/// Hash the part already on disk (blocking; stops when cancelled).
fn hash_prefix(path: &Path, len: u64, cancel: &CancellationToken) -> Result<Sha256> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 4 * 1024 * 1024];
    let mut left = len;
    while left > 0 {
        ensure!(!cancel.is_cancelled(), "Stopped");
        let want = buffer.len().min(left as usize);
        let read = file.read(&mut buffer[..want])?;
        ensure!(read > 0, "The partial download is shorter than expected");
        hasher.update(&buffer[..read]);
        left -= read as u64;
    }
    Ok(hasher)
}

/// `Content-Range: bytes START-END/TOTAL` → (START, TOTAL).
fn content_range(response: &reqwest::Response) -> Option<(u64, u64)> {
    let value = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?;
    let rest = value.strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, _) = range.split_once('-')?;
    Some((start.trim().parse().ok()?, total.trim().parse().ok()?))
}

/// What a running download keeps checking: Pause/Cancel, and its guard.
struct Checks<'a> {
    cancel: &'a CancellationToken,
    guard: &'a Guard,
    every: Duration,
}

/// Download `spec` into `dir`, resuming `<file>.part` when it exists. The
/// file gets its final name only after its size and SHA-256 match.
async fn fetch(
    spec: &Spec,
    dir: &Path,
    client: &reqwest::Client,
    checks: Checks<'_>,
    mut report: impl FnMut(State, u64),
) -> Result<()> {
    let Checks {
        cancel,
        guard,
        every,
    } = checks;
    let part = part_path(dir, &spec.file);
    let target = final_path(dir, &spec.file);
    let mut restarted = false;
    'attempt: loop {
        let mut have = partial_len(dir, &spec.file);
        if have > spec.bytes {
            fs::remove_file(&part)?;
            have = 0;
        }
        let mut hasher = if have > 0 {
            report(State::Checking, have);
            let (path, token) = (part.clone(), cancel.clone());
            tokio::task::spawn_blocking(move || hash_prefix(&path, have, &token))
                .await
                .context("The resume check stopped")??
        } else {
            Sha256::new()
        };
        ensure!(!cancel.is_cancelled(), "Stopped");
        guard().map_err(|reason| anyhow::anyhow!(reason))?;
        if have < spec.bytes {
            report(State::Downloading, have);
            let mut request = client.get(&spec.url);
            if have > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
            }
            let response = tokio::select! {
                _ = cancel.cancelled() => bail!("Stopped"),
                response = request.send() => response.context(
                    "Could not reach the download server. Check the internet connection, then choose Resume.",
                )?,
            };
            let status = response.status().as_u16();
            match status {
                206 if have > 0 => {
                    let range = content_range(&response);
                    if range != Some((have, spec.bytes)) {
                        // Not the part we asked for: start over once.
                        ensure!(
                            !restarted,
                            "The server sent a different part of the file than requested"
                        );
                        restarted = true;
                        fs::remove_file(&part)?;
                        continue 'attempt;
                    }
                }
                200 => {
                    if have > 0 {
                        // The server ignored the range: start from the beginning.
                        have = 0;
                        hasher = Sha256::new();
                        fs::File::create(&part)
                            .map_err(|e| plain_io(e, "Cannot write the download"))?;
                        report(State::Downloading, 0);
                    }
                    if let Some(length) = response.content_length() {
                        ensure!(
                            length == spec.bytes,
                            "The server offers {length} bytes; expected {}",
                            spec.bytes
                        );
                    }
                }
                416 if !restarted => {
                    restarted = true;
                    fs::remove_file(&part)?;
                    continue 'attempt;
                }
                _ => bail!("The download server answered HTTP {status}. Try again later."),
            }
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&part)
                .map_err(|e| plain_io(e, "Cannot write the download"))?;
            let mut done = have;
            let mut stream = futures_util::StreamExt::fuse(response.bytes_stream());
            let mut last_report = Instant::now();
            let mut last_guard = Instant::now();
            loop {
                let next = tokio::select! {
                    _ = cancel.cancelled() => {
                        let _ = file.flush();
                        bail!("Stopped");
                    }
                    next = tokio::time::timeout(STALL_TIMEOUT, futures_util::StreamExt::next(&mut stream)) => next,
                };
                let Ok(next) = next else {
                    bail!(
                        "The connection stalled at {} of {}. Choose Resume to continue.",
                        gb(done),
                        gb(spec.bytes)
                    );
                };
                let Some(chunk) = next else {
                    break;
                };
                let chunk = chunk.map_err(|_| {
                    anyhow::anyhow!(
                        "The connection dropped at {} of {}. Choose Resume to continue.",
                        gb(done),
                        gb(spec.bytes)
                    )
                })?;
                if done + chunk.len() as u64 > spec.bytes {
                    drop(file);
                    let _ = fs::remove_file(&part);
                    bail!("The download is larger than expected and was deleted");
                }
                file.write_all(&chunk)
                    .map_err(|e| plain_io(e, "Cannot write the download"))?;
                hasher.update(&chunk);
                done += chunk.len() as u64;
                if last_report.elapsed() >= Duration::from_millis(250) {
                    report(State::Downloading, done);
                    last_report = Instant::now();
                }
                if last_guard.elapsed() >= every {
                    last_guard = Instant::now();
                    if let Err(reason) = guard() {
                        let _ = file.flush();
                        bail!("{reason}");
                    }
                }
            }
            file.sync_all()
                .map_err(|e| plain_io(e, "Cannot write the download"))?;
            report(State::Downloading, done);
            ensure!(
                done == spec.bytes,
                "The connection closed early at {} of {}. Choose Resume to continue.",
                gb(done),
                gb(spec.bytes)
            );
        }
        let digest = format!("{:x}", hasher.finalize());
        if digest != spec.sha256 {
            let _ = fs::remove_file(&part);
            bail!(
                "The downloaded file did not match its checksum and was deleted. Try again; if it happens again, the download source may be damaged."
            );
        }
        fs::rename(&part, &target)
            .with_context(|| format!("Could not move the download to {}", target.display()))?;
        return Ok(());
    }
}

/// `GET /api/local-models/downloads`: the catalog with each model's fit on
/// this computer, the recommendation, and download progress.
pub fn catalog_json(
    downloader: &Downloader,
    dir: &Path,
    ram_bytes: u64,
    gpu: Option<(&str, u64)>,
    architectures: Option<&std::collections::HashSet<String>>,
    offline: bool,
) -> serde_json::Value {
    let vram = gpu.map(|(_, bytes)| bytes);
    let supported =
        |model: &CatalogModel| architectures.is_none_or(|a| a.contains(model.architecture));
    let recommended = recommend_among(ram_bytes, vram, supported);
    let free = downloader.free_space(dir);
    let models: Vec<serde_json::Value> = CATALOG
        .iter()
        .map(|model| {
            let status = downloader.status(&model.spec(), dir);
            let supported = supported(model);
            let path = final_path(dir, model.file);
            let installed = status.state == State::Installed;
            serde_json::json!({
                "id": model.id,
                "name": model.name,
                "publisher": model.publisher,
                "summary": model.summary,
                "file": model.file,
                "bytes": model.bytes,
                "sha256": model.sha256,
                "license": model.license,
                "license_url": model.license_url,
                "source_url": model.source_url(),
                "quantization": model.quantization,
                "architecture": model.architecture,
                "memory_bytes": model.memory_bytes,
                "min_memory_bytes": model.min_memory_bytes,
                "fit": fit(model, ram_bytes, vram),
                "recommended": recommended.is_some_and(|(r, _)| r.id == model.id),
                "supported": supported,
                "unsupported_reason": (!supported).then(|| format!(
                    "The bundled llama.cpp can't run the {} architecture",
                    model.architecture
                )),
                "state": status.state,
                "done": status.done,
                "total": status.total,
                "bytes_per_second": status.bytes_per_second,
                "error": status.error,
                "model_id": installed.then(|| crate::local_engine::entry_id(&path)),
                "path": installed.then(|| path.display().to_string()),
            })
        })
        .collect();
    serde_json::json!({
        "directory": dir,
        "free_bytes": free,
        "offline": offline,
        "hardware": {
            "ram_bytes": ram_bytes,
            "vram_bytes": vram,
            "gpu": gpu.map(|(name, _)| name),
        },
        "recommended": recommended.map(|(m, _)| m.id),
        "recommended_fit": recommended.map(|(_, f)| f),
        "busy": downloader.busy(),
        "models": models,
    })
}

#[cfg(test)]
mod tests;

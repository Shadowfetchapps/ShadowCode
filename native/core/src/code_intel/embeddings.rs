//! Optional semantic search: a small embedding GGUF run by the bundled
//! llama.cpp (`llama-server --embedding`), with vectors in plain SQLite.
//!
//! Nothing here downloads on its own. A model is fetched only when the user
//! clicks Install in Settings; the URL is pinned to a Hugging Face commit and
//! the file must match its recorded size and SHA-256. The embedding server is
//! a separate llama-server process (the chat model keeps its own), bound to
//! 127.0.0.1 with a random key, stopped after a few idle minutes.
//!
//! Vectors are keyed by (model, chunk digest), so they survive restarts and
//! are shared by every project; brute-force cosine is fine at repo scale.
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub license: &'static str,
    pub dims: usize,
    /// Tokens per input the model was trained for.
    pub context: u32,
    /// Parallel sequences the server runs.
    pub parallel: u32,
    /// Inputs are cut to this many characters (stays inside `context`).
    pub max_chars: usize,
    pub query_prefix: &'static str,
    pub document_prefix: &'static str,
    pub summary: &'static str,
}

pub const CATALOG: &[ModelEntry] = &[
    ModelEntry {
        id: "bge-small-en-v1.5-q8",
        name: "BGE small (English) v1.5, Q8_0",
        file: "bge-small-en-v1.5-q8_0.gguf",
        url: "https://huggingface.co/CompendiumLabs/bge-small-en-v1.5-gguf/resolve/d32f8c040ea3b516330eeb75b72bcc2d3a780ab7/bge-small-en-v1.5-q8_0.gguf",
        sha256: "ec38e8da142596baa913124ae50550de284b6916bf59577ef2f0cb9660c2f514",
        bytes: 36_806_944,
        license: "MIT",
        dims: 384,
        context: 512,
        parallel: 4,
        max_chars: 1_200,
        query_prefix: "Represent this sentence for searching relevant passages: ",
        document_prefix: "",
        summary: "Smallest and fastest. Good for finding code by what it does.",
    },
    ModelEntry {
        id: "nomic-embed-text-v1.5-q8",
        name: "Nomic Embed Text v1.5, Q8_0",
        file: "nomic-embed-text-v1.5.Q8_0.gguf",
        url: "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5-GGUF/resolve/0188c9bf409793f810680a5a431e7b899c46104c/nomic-embed-text-v1.5.Q8_0.gguf",
        sha256: "3e24342164b3d94991ba9692fdc0dd08e3fd7362e0aacc396a9a5c54a544c3b7",
        bytes: 146_146_432,
        license: "Apache-2.0",
        dims: 768,
        context: 2_048,
        parallel: 2,
        max_chars: 5_000,
        query_prefix: "search_query: ",
        document_prefix: "search_document: ",
        summary: "Larger; reads whole chunks instead of their first lines.",
    },
];

pub fn catalog_entry(id: &str) -> Option<&'static ModelEntry> {
    CATALOG.iter().find(|m| m.id == id)
}

pub fn model_path(data_dir: &Path, entry: &ModelEntry) -> PathBuf {
    data_dir.join("models").join(entry.file)
}

pub fn installed(data_dir: &Path, entry: &ModelEntry) -> bool {
    std::fs::metadata(model_path(data_dir, entry)).is_ok_and(|m| m.len() == entry.bytes)
}

/// The model search_code uses, if semantic search is on and one is installed.
pub fn active(data_dir: &Path, config: &super::CodeIntelConfig) -> Option<&'static ModelEntry> {
    if !config.semantic_search {
        return None;
    }
    if let Some(entry) = catalog_entry(&config.embedding_model) {
        return installed(data_dir, entry).then_some(entry);
    }
    CATALOG.iter().find(|entry| installed(data_dir, entry))
}

// ---------------------------------------------------------------------------
// Download (only on request)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub state: String,
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
}

static PROGRESS: Mutex<Option<HashMap<String, Progress>>> = Mutex::new(None);

fn set_progress(id: &str, progress: Progress) {
    if let Ok(mut map) = PROGRESS.lock() {
        map.get_or_insert_with(HashMap::new)
            .insert(id.to_owned(), progress);
    }
}

pub fn progress(id: &str) -> Option<Progress> {
    PROGRESS.lock().ok()?.as_ref()?.get(id).cloned()
}

/// Start a background download. Returns false when one is already running.
pub fn start_install(
    data_dir: PathBuf,
    entry: &'static ModelEntry,
    on_done: impl FnOnce(Result<()>) + Send + 'static,
) -> bool {
    if progress(entry.id).is_some_and(|p| matches!(p.state.as_str(), "downloading" | "verifying")) {
        return false;
    }
    set_progress(
        entry.id,
        Progress {
            state: "downloading".into(),
            total: entry.bytes,
            ..Default::default()
        },
    );
    tokio::spawn(async move {
        let result = download(
            entry.url,
            entry.sha256,
            entry.bytes,
            &model_path(&data_dir, entry),
            |done| {
                set_progress(
                    entry.id,
                    Progress {
                        state: "downloading".into(),
                        done,
                        total: entry.bytes,
                        error: None,
                    },
                )
            },
        )
        .await;
        set_progress(
            entry.id,
            match &result {
                Ok(()) => Progress {
                    state: "installed".into(),
                    done: entry.bytes,
                    total: entry.bytes,
                    error: None,
                },
                Err(error) => Progress {
                    state: "error".into(),
                    done: 0,
                    total: entry.bytes,
                    error: Some(format!("{error:#}")),
                },
            },
        );
        on_done(result);
    });
    true
}

/// A download that receives nothing for this long has stalled (a dropped
/// network, a suspended laptop): it fails, and can be started again.
pub(crate) const DOWNLOAD_STALL: Duration = Duration::from_secs(60);

/// The next piece of a download, or a clear error when none arrives in time.
pub(crate) async fn next_piece<S, T>(stream: &mut S) -> Result<Option<T>>
where
    S: futures_util::Stream<Item = reqwest::Result<T>> + Unpin,
{
    match tokio::time::timeout(DOWNLOAD_STALL, futures_util::StreamExt::next(stream)).await {
        Err(_) => bail!(
            "The download stalled: nothing arrived for {} seconds. Check the connection and try again.",
            DOWNLOAD_STALL.as_secs()
        ),
        Ok(None) => Ok(None),
        Ok(Some(piece)) => Ok(Some(piece.context("The download was interrupted")?)),
    }
}

/// Stream `url` to `target`, checking the exact size and SHA-256 before the
/// file appears under its final name.
pub async fn download(
    url: &str,
    sha256: &str,
    bytes: u64,
    target: &Path,
    mut progress: impl FnMut(u64),
) -> Result<()> {
    let parent = target.parent().context("Model path has no parent")?;
    crate::paths::private_directory(parent)?;
    let partial = parent.join(format!(".download-{}", crate::id()));
    let result = async {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .user_agent(concat!("ShadowCode/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let response = tokio::time::timeout(DOWNLOAD_STALL, client.get(url).send())
            .await
            .context("huggingface.co did not answer")?
            .context("Could not reach huggingface.co")?;
        ensure!(
            response.status().is_success(),
            "The download returned HTTP {}",
            response.status().as_u16()
        );
        let mut file = std::fs::File::create(&partial)?;
        let mut hasher = Sha256::new();
        let mut done = 0u64;
        let mut stream = futures_util::StreamExt::fuse(response.bytes_stream());
        let mut last_report = Instant::now();
        while let Some(chunk) = next_piece(&mut stream).await? {
            done += chunk.len() as u64;
            ensure!(done <= bytes, "The download is larger than expected");
            hasher.update(&chunk);
            file.write_all(&chunk)?;
            if last_report.elapsed() > Duration::from_millis(250) {
                progress(done);
                last_report = Instant::now();
            }
        }
        file.flush()?;
        drop(file);
        ensure!(
            done == bytes,
            "The download is {done} bytes; expected {bytes}"
        );
        let digest = format!("{:x}", hasher.finalize());
        ensure!(
            digest == sha256,
            "The download's SHA-256 is {digest}; expected {sha256}"
        );
        std::fs::rename(&partial, target)?;
        Ok(())
    }
    .await;
    let _ = std::fs::remove_file(&partial);
    result
}

pub fn remove(data_dir: &Path, entry: &ModelEntry) -> Result<bool> {
    let path = model_path(data_dir, entry);
    let existed = path.exists();
    if existed {
        std::fs::remove_file(&path)?;
    }
    let db = vectors_path(data_dir);
    if db.exists() {
        open_vectors(data_dir)?.execute("DELETE FROM vectors WHERE model=?", [entry.id])?;
    }
    if let Ok(mut map) = PROGRESS.lock() {
        if let Some(map) = map.as_mut() {
            map.remove(entry.id);
        }
    }
    Ok(existed)
}

// ---------------------------------------------------------------------------
// Vector store
// ---------------------------------------------------------------------------

/// Cached vectors per model beyond this are pruned oldest first.
const MAX_VECTORS_PER_MODEL: i64 = 60_000;
const VECTOR_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS vectors (
  model TEXT NOT NULL,
  digest TEXT NOT NULL,
  dims INTEGER NOT NULL,
  vec BLOB NOT NULL,
  created_at REAL NOT NULL,
  PRIMARY KEY(model, digest)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_vectors_age ON vectors(model, created_at);
"#;

pub fn vectors_path(data_dir: &Path) -> PathBuf {
    data_dir.join("vectors.sqlite")
}

fn open_vectors(data_dir: &Path) -> Result<Connection> {
    crate::paths::private_directory(data_dir)?;
    let conn = Connection::open(vectors_path(data_dir))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    conn.execute_batch(VECTOR_SCHEMA)?;
    Ok(conn)
}

/// The project index with the shared vector store attached as `vdb`.
fn index_with_vectors(root: &Path, data_dir: &Path) -> Result<Connection> {
    drop(open_vectors(data_dir)?);
    let conn = crate::symbol_index::open_index(root)?;
    conn.execute(
        "ATTACH DATABASE ?1 AS vdb",
        [vectors_path(data_dir).to_string_lossy()],
    )?;
    Ok(conn)
}

pub fn encode(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|v| v.to_le_bytes()).collect()
}

pub fn decode(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

pub fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        vector.iter_mut().for_each(|v| *v /= norm);
    }
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub fn store_vectors(data_dir: &Path, model: &str, rows: &[(String, Vec<f32>)]) -> Result<()> {
    let mut conn = open_vectors(data_dir)?;
    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare_cached(
            "INSERT OR REPLACE INTO vectors(model, digest, dims, vec, created_at) VALUES(?,?,?,?,?)",
        )?;
        for (digest, vector) in rows {
            insert.execute(params![
                model,
                digest,
                vector.len() as i64,
                encode(vector),
                crate::now()
            ])?;
        }
    }
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM vectors WHERE model=?", [model], |r| {
        r.get(0)
    })?;
    if count > MAX_VECTORS_PER_MODEL {
        tx.execute(
            "DELETE FROM vectors WHERE model=?1 AND digest IN (SELECT digest FROM vectors WHERE model=?1 ORDER BY created_at LIMIT ?2)",
            params![model, count - MAX_VECTORS_PER_MODEL],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Chunks of this project that have no vector for `model` yet.
pub fn missing(
    root: &Path,
    data_dir: &Path,
    entry: &ModelEntry,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    let conn = index_with_vectors(root, data_dir)?;
    let mut stmt = conn.prepare(
        "SELECT c.digest, f.path, f.symbols, f.body FROM chunks c JOIN chunks_fts f ON f.rowid = c.id
         WHERE NOT EXISTS (SELECT 1 FROM vdb.vectors v WHERE v.model = ?1 AND v.digest = c.digest)
         GROUP BY c.digest LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![entry.id, limit as i64], |r| {
            let text = super::chunks::embed_text(
                &r.get::<_, String>(1)?,
                &r.get::<_, String>(2)?,
                &r.get::<_, String>(3)?,
            );
            Ok((r.get::<_, String>(0)?, text))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// (chunks with a vector, all chunks) for this project and model.
pub fn coverage(root: &Path, data_dir: &Path, entry: &ModelEntry) -> Result<(i64, i64)> {
    let conn = index_with_vectors(root, data_dir)?;
    Ok(conn.query_row(
        "SELECT
           (SELECT COUNT(*) FROM chunks c WHERE EXISTS (SELECT 1 FROM vdb.vectors v WHERE v.model=?1 AND v.digest=c.digest)),
           (SELECT COUNT(*) FROM chunks)",
        [entry.id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// Chunk ids ranked by cosine similarity to `query` (best first).
pub fn rank(
    root: &Path,
    data_dir: &Path,
    entry: &ModelEntry,
    query: &[f32],
    path_prefix: &str,
    limit: usize,
) -> Result<Vec<(i64, f32)>> {
    let conn = index_with_vectors(root, data_dir)?;
    let mut stmt = conn.prepare(
        "SELECT c.id, v.vec FROM chunks c JOIN vdb.vectors v ON v.model = ?1 AND v.digest = c.digest
         WHERE substr(c.path, 1, length(?2)) = ?2",
    )?;
    let mut scored = stmt
        .query_map(params![entry.id, path_prefix], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?
        .filter_map(|row| row.ok())
        .filter_map(|(id, blob)| {
            let vector = decode(&blob);
            (vector.len() == query.len()).then(|| (id, dot(query, &vector)))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(limit);
    Ok(scored)
}

// ---------------------------------------------------------------------------
// Embedding server
// ---------------------------------------------------------------------------

const IDLE_STOP: Duration = Duration::from_secs(300);
const STARTUP: Duration = Duration::from_secs(60);

struct Server {
    child: tokio::process::Child,
    port: u16,
    key: String,
    model: PathBuf,
    last_used: Instant,
}

static SERVER: tokio::sync::Mutex<Option<Server>> = tokio::sync::Mutex::const_new(None);

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

pub fn server_args(entry: &ModelEntry, model: &Path, port: u16, gpu: bool) -> Vec<String> {
    let total = (entry.context * entry.parallel).to_string();
    let mut args: Vec<String> = vec![
        "-m".into(),
        model.display().to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--no-webui".into(),
        "--embedding".into(),
        "--ctx-size".into(),
        total.clone(),
        "--batch-size".into(),
        total.clone(),
        "--ubatch-size".into(),
        total,
        "--parallel".into(),
        entry.parallel.to_string(),
    ];
    if gpu {
        args.extend(["-ngl".into(), "999".into()]);
    } else {
        args.extend(["--device".into(), "none".into(), "-ngl".into(), "0".into()]);
    }
    args
}

async fn launch(binary: &Path, entry: &ModelEntry, model: &Path, gpu: bool) -> Result<Server> {
    let port = free_port()?;
    let key = crate::local_runtime::random_key()?;
    let mut command = tokio::process::Command::new(binary);
    command
        .args(server_args(entry, model, port, gpu))
        .env_clear()
        .envs(crate::local_engine::runtime_env(binary))
        .env("LLAMA_API_KEY", &key)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
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
    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to start {}", binary.display()))?;
    let client = crate::local_runtime::loopback_client(Duration::from_secs(2))?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("The embedding server exited before it was ready ({status})");
        }
        let healthy = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .bearer_auth(&key)
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        if healthy {
            break;
        }
        if started.elapsed() > STARTUP {
            let _ = child.kill().await;
            bail!(
                "The embedding server did not start within {}s",
                STARTUP.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    Ok(Server {
        child,
        port,
        key,
        model: model.to_owned(),
        last_used: Instant::now(),
    })
}

fn spawn_idle_stop() {
    tokio::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let mut guard = SERVER.lock().await;
            let Some(server) = guard.as_mut() else {
                return;
            };
            if server.last_used.elapsed() > IDLE_STOP
                || server.child.try_wait().ok().flatten().is_some()
            {
                let _ = server.child.kill().await;
                *guard = None;
                return;
            }
        }
    });
}

pub async fn stop() -> bool {
    let mut guard = SERVER.lock().await;
    match guard.take() {
        Some(mut server) => {
            let _ = server.child.kill().await;
            true
        }
        None => false,
    }
}

pub async fn running() -> Option<Value> {
    let guard = SERVER.lock().await;
    guard.as_ref().map(|s| {
        json!({
            "model": s.model.file_name().map(|n| n.to_string_lossy().into_owned()),
            "pid": s.child.id(),
            "idle_sec": s.last_used.elapsed().as_secs(),
        })
    })
}

/// Everything needed to embed with one model.
#[derive(Clone, Debug)]
pub struct Semantic {
    pub data_dir: PathBuf,
    pub entry: &'static ModelEntry,
    pub binary: PathBuf,
}

impl Semantic {
    pub fn resolve(data_dir: &Path, config: &crate::config::Config) -> Option<Self> {
        let intel = super::CodeIntelConfig::lenient(config);
        let entry = active(data_dir, &intel)?;
        let binary = crate::local_engine::runtime_candidates(&config.local_engine.llama_binary)
            .into_iter()
            .next()?
            .0;
        Some(Self {
            data_dir: data_dir.to_owned(),
            entry,
            binary,
        })
    }
}

fn truncate_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

/// Embed texts (prefixes and truncation applied here). Vectors are unit length.
pub async fn embed(semantic: &Semantic, texts: &[String], query: bool) -> Result<Vec<Vec<f32>>> {
    let entry = semantic.entry;
    let model = model_path(&semantic.data_dir, entry);
    ensure!(
        installed(&semantic.data_dir, entry),
        "The embedding model is not installed"
    );
    let inputs: Vec<String> = texts
        .iter()
        .map(|text| {
            let prefix = if query {
                entry.query_prefix
            } else {
                entry.document_prefix
            };
            format!("{prefix}{}", truncate_chars(text, entry.max_chars))
        })
        .collect();
    let mut guard = SERVER.lock().await;
    let stale = match guard.as_mut() {
        Some(server) => server.model != model || server.child.try_wait().ok().flatten().is_some(),
        None => true,
    };
    if stale {
        if let Some(mut old) = guard.take() {
            let _ = old.child.kill().await;
        }
        let server = match launch(&semantic.binary, entry, &model, true).await {
            Ok(server) => server,
            // The GPU may be full with the chat model; the CPU still works.
            Err(_) => launch(&semantic.binary, entry, &model, false).await?,
        };
        *guard = Some(server);
        spawn_idle_stop();
    }
    let server = guard.as_mut().context("Embedding server unavailable")?;
    server.last_used = Instant::now();
    let client = crate::local_runtime::loopback_client(Duration::from_secs(120))?;
    let response = client
        .post(format!("http://127.0.0.1:{}/v1/embeddings", server.port))
        .bearer_auth(&server.key)
        .json(&json!({"input": inputs, "model": entry.id, "encoding_format": "float"}))
        .send()
        .await
        .context("The embedding server did not answer")?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .context("Invalid embedding response")?;
    ensure!(
        status.is_success(),
        "The embedding server answered HTTP {}: {}",
        status.as_u16(),
        crate::tools::truncate(&body.to_string(), 400)
    );
    let mut data = body["data"].as_array().cloned().unwrap_or_default();
    data.sort_by_key(|item| item["index"].as_u64().unwrap_or(0));
    ensure!(
        data.len() == texts.len(),
        "The embedding server returned {} vectors for {} inputs",
        data.len(),
        texts.len()
    );
    data.iter()
        .map(|item| {
            let mut vector: Vec<f32> = item["embedding"]
                .as_array()
                .context("Embedding is not an array")?
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect();
            ensure!(!vector.is_empty(), "Empty embedding");
            normalize(&mut vector);
            Ok(vector)
        })
        .collect()
}

/// Embed chunks that have no vector yet, until done or `budget` runs out.
/// Returns how many chunks were embedded now.
pub async fn fill(root: &Path, semantic: &Semantic, budget: Duration) -> Result<usize> {
    let started = Instant::now();
    let mut embedded = 0usize;
    let batch = (semantic.entry.parallel as usize * 8).max(8);
    while started.elapsed() < budget {
        let (root_owned, data_dir, entry) =
            (root.to_owned(), semantic.data_dir.clone(), semantic.entry);
        let rows =
            tokio::task::spawn_blocking(move || missing(&root_owned, &data_dir, entry, batch))
                .await
                .context("Embedding worker stopped")??;
        if rows.is_empty() {
            break;
        }
        let texts: Vec<String> = rows.iter().map(|(_, text)| text.clone()).collect();
        let vectors = embed(semantic, &texts, false).await?;
        let stored: Vec<(String, Vec<f32>)> = rows
            .into_iter()
            .map(|(digest, _)| digest)
            .zip(vectors)
            .collect();
        embedded += stored.len();
        let (data_dir, id) = (semantic.data_dir.clone(), semantic.entry.id);
        tokio::task::spawn_blocking(move || store_vectors(&data_dir, id, &stored))
            .await
            .context("Embedding worker stopped")??;
    }
    Ok(embedded)
}

/// Background backfill per project, one at a time per project.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Backfill {
    pub state: String,
    pub embedded: usize,
    pub error: Option<String>,
}

static BACKFILL: Mutex<Option<HashMap<PathBuf, Backfill>>> = Mutex::new(None);

pub fn backfill_status(root: &Path) -> Option<Backfill> {
    BACKFILL.lock().ok()?.as_ref()?.get(root).cloned()
}

fn set_backfill(root: &Path, value: Backfill) {
    if let Ok(mut map) = BACKFILL.lock() {
        map.get_or_insert_with(HashMap::new)
            .insert(root.to_owned(), value);
    }
}

/// Index the project and embed every chunk in the background.
pub fn spawn_backfill(root: PathBuf, semantic: Semantic) -> bool {
    if backfill_status(&root).is_some_and(|b| b.state == "running") {
        return false;
    }
    set_backfill(
        &root,
        Backfill {
            state: "running".into(),
            ..Default::default()
        },
    );
    let shared = Arc::new(root);
    tokio::spawn(async move {
        let root = shared.as_ref().clone();
        let scan_root = root.clone();
        let indexed = tokio::task::spawn_blocking(move || {
            crate::symbol_index::ensure_index(&scan_root, &[], true)
        })
        .await;
        if let Err(error) = indexed
            .map_err(anyhow::Error::from)
            .and_then(|r| r.map(|_| ()))
        {
            set_backfill(
                &root,
                Backfill {
                    state: "error".into(),
                    embedded: 0,
                    error: Some(format!("{error:#}")),
                },
            );
            return;
        }
        let mut total = 0usize;
        loop {
            match fill(&root, &semantic, Duration::from_secs(20)).await {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    set_backfill(
                        &root,
                        Backfill {
                            state: "running".into(),
                            embedded: total,
                            error: None,
                        },
                    );
                }
                Err(error) => {
                    set_backfill(
                        &root,
                        Backfill {
                            state: "error".into(),
                            embedded: total,
                            error: Some(format!("{error:#}")),
                        },
                    );
                    return;
                }
            }
        }
        set_backfill(
            &root,
            Backfill {
                state: "done".into(),
                embedded: total,
                error: None,
            },
        );
    });
    true
}

pub fn catalog_json(data_dir: &Path, config: &super::CodeIntelConfig) -> Value {
    let active = active(data_dir, config).map(|m| m.id);
    json!(CATALOG
        .iter()
        .map(|entry| json!({
            "id": entry.id,
            "name": entry.name,
            "summary": entry.summary,
            "bytes": entry.bytes,
            "license": entry.license,
            "sha256": entry.sha256,
            "url": entry.url,
            "dims": entry.dims,
            "installed": installed(data_dir, entry),
            "active": active == Some(entry.id),
            "progress": progress(entry.id),
        }))
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A download whose server goes silent fails with a clear message
    /// instead of showing "downloading" forever. Paused clock: no real wait.
    #[tokio::test(start_paused = true)]
    async fn a_silent_download_fails_instead_of_hanging() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/model.bin", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nabc")
                .await
                .unwrap();
            // Then nothing, for an hour.
            tokio::time::sleep(Duration::from_secs(3600)).await;
            drop(socket);
        });
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("model.bin");
        let error = download(&url, "0", 1000, &target, |_| {})
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("stalled"), "{error}");
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        server.abort();
    }

    #[test]
    fn vectors_round_trip_and_normalize() {
        let mut v = vec![3.0f32, 4.0];
        normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert_eq!(decode(&encode(&v)), v);
        assert!((dot(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn catalog_is_pinned() {
        for entry in CATALOG {
            assert_eq!(entry.sha256.len(), 64);
            assert!(entry.url.starts_with("https://huggingface.co/"));
            // Pinned to a commit, never a moving branch.
            assert!(!entry.url.contains("/resolve/main/"));
            assert!(entry.url.ends_with(entry.file));
            assert!(entry.bytes > 1_000_000);
        }
        assert!(catalog_entry("bge-small-en-v1.5-q8").is_some());
    }

    #[test]
    fn vector_store_ranks_by_cosine_and_reports_missing_chunks() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.md"), "alpha words\n").unwrap();
        std::fs::write(root.path().join("b.md"), "beta words\n").unwrap();
        crate::symbol_index::ensure_index(root.path(), &[], true).unwrap();
        let entry = &CATALOG[0];
        let pending = missing(root.path(), data.path(), entry, 10).unwrap();
        assert_eq!(pending.len(), 2);
        let rows: Vec<(String, Vec<f32>)> = pending
            .iter()
            .map(|(digest, text)| {
                let v = if text.starts_with("a.md") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                };
                (digest.clone(), v)
            })
            .collect();
        store_vectors(data.path(), entry.id, &rows).unwrap();
        assert!(missing(root.path(), data.path(), entry, 10)
            .unwrap()
            .is_empty());
        assert_eq!(coverage(root.path(), data.path(), entry).unwrap(), (2, 2));
        let ranked = rank(root.path(), data.path(), entry, &[0.1, 0.9], "", 5).unwrap();
        assert_eq!(ranked.len(), 2);
        let conn = crate::symbol_index::open_index(root.path()).unwrap();
        let best: String = conn
            .query_row("SELECT path FROM chunks WHERE id=?", [ranked[0].0], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(best, "b.md");
        assert!(
            rank(root.path(), data.path(), entry, &[0.1, 0.9], "a", 5)
                .unwrap()
                .len()
                == 1
        );
    }
}

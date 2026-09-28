//! A JSON-RPC (LSP) client over a language server's stdin/stdout.
//!
//! One reader task parses `Content-Length` frames, completes pending
//! requests, stores `textDocument/publishDiagnostics`, and answers the few
//! server-to-client requests servers block on (configuration, capability
//! registration, progress tokens). Memory is bounded: frame size, stored
//! diagnostics per file and in total, open documents, and stderr.
use super::servers::{self, Launch};
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::{oneshot, Notify},
};

const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
const MAX_DIAGNOSTIC_FILES: usize = 1_024;
const MAX_DIAGNOSTICS_PER_FILE: usize = 200;
const MAX_OPEN_DOCUMENTS: usize = 48;
const STDERR_BYTES: usize = 8 * 1024;
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);

/// Diagnostics a server published for one document.
#[derive(Clone, Debug)]
pub struct Published {
    pub seq: u64,
    pub version: Option<i64>,
    pub items: Vec<Value>,
    pub at: Instant,
}

#[derive(Default)]
struct Diagnostics {
    seq: u64,
    by_uri: HashMap<String, Published>,
}

struct Shared {
    writer: tokio::sync::Mutex<ChildStdin>,
    pending: Mutex<HashMap<i64, oneshot::Sender<std::result::Result<Value, String>>>>,
    diagnostics: Mutex<Diagnostics>,
    changed: Notify,
    alive: AtomicBool,
    /// rust-analyzer reports when its workspace is loaded; others are ready
    /// once initialized.
    ready: AtomicBool,
    settings: Value,
    root_uri: String,
    stderr: Mutex<VecDeque<u8>>,
    exit: Mutex<Option<String>>,
}

struct Document {
    version: i64,
    digest: String,
    used: Instant,
}

pub struct Client {
    pub name: String,
    pub root: PathBuf,
    pub launch: Launch,
    pub started: Instant,
    pid: Option<u32>,
    child: Mutex<Option<Child>>,
    shared: Arc<Shared>,
    next_id: AtomicI64,
    documents: tokio::sync::Mutex<HashMap<String, Document>>,
    last_used: Mutex<Instant>,
    capabilities: Mutex<Value>,
}

pub fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn uri_for(path: &Path) -> String {
    format!("file://{}", percent_encode(&path.to_string_lossy()))
}

pub fn path_for(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// UTF-16 offset of a character column in a line (LSP's default encoding).
pub fn utf16_column(line: &str, chars: usize) -> usize {
    line.chars().take(chars).map(char::len_utf16).sum()
}

/// Character column of a UTF-16 offset in a line.
pub fn char_column(line: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (index, ch) in line.chars().enumerate() {
        if units >= utf16 {
            return index;
        }
        units += ch.len_utf16();
    }
    line.chars().count()
}

/// A server that stops reading its input fills the pipe; a write that cannot
/// finish in this time gives up instead of blocking the tool (and Stop).
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

async fn write_frame(writer: &tokio::sync::Mutex<ChildStdin>, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    tokio::time::timeout(WRITE_TIMEOUT, async {
        let mut writer = writer.lock().await;
        writer
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await?;
        writer.write_all(&body).await?;
        writer.flush().await?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .map_err(|_| anyhow!("The language server stopped reading its input"))?
}

async fn read_frame(reader: &mut BufReader<ChildStdout>) -> Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut line = Vec::new();
        let read = (&mut *reader)
            .take(4096)
            .read_until(b'\n', &mut line)
            .await?;
        if read == 0 {
            return Ok(None);
        }
        ensure!(
            line.ends_with(b"\n"),
            "Language server header line is too long"
        );
        let line = String::from_utf8_lossy(&line);
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>()?);
            }
        }
    }
    let length = length.context("Language server message has no Content-Length")?;
    ensure!(
        length <= MAX_FRAME_BYTES,
        "Language server message is too large"
    );
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

impl Shared {
    fn store(&self, params: &Value) {
        let Some(uri) = params["uri"].as_str() else {
            return;
        };
        let mut items = params["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        items.truncate(MAX_DIAGNOSTICS_PER_FILE);
        if let Ok(mut diagnostics) = self.diagnostics.lock() {
            diagnostics.seq += 1;
            let seq = diagnostics.seq;
            diagnostics.by_uri.insert(
                uri.to_owned(),
                Published {
                    seq,
                    version: params["version"].as_i64(),
                    items,
                    at: Instant::now(),
                },
            );
            if diagnostics.by_uri.len() > MAX_DIAGNOSTIC_FILES {
                if let Some(oldest) = diagnostics
                    .by_uri
                    .iter()
                    .min_by_key(|(_, p)| p.seq)
                    .map(|(k, _)| k.clone())
                {
                    diagnostics.by_uri.remove(&oldest);
                }
            }
        }
        self.changed.notify_waiters();
    }

    fn configuration(&self, params: &Value) -> Value {
        let items = params["items"].as_array().cloned().unwrap_or_default();
        json!(items
            .iter()
            .map(|item| {
                let section = item["section"].as_str().unwrap_or("");
                self.settings.get(section).cloned().unwrap_or(Value::Null)
            })
            .collect::<Vec<_>>())
    }

    fn handle(self: &Arc<Self>, message: Value) {
        if std::env::var_os("SHADOWCODE_LSP_TRACE").is_some() {
            eprintln!(
                "LSP<< {}",
                crate::tools::truncate(&message.to_string(), 600)
            );
        }
        let method = message["method"].as_str();
        match (method, message.get("id")) {
            (Some(method), Some(id)) => {
                let result = match method {
                    "workspace/configuration" => Ok(self.configuration(&message["params"])),
                    "client/registerCapability"
                    | "client/unregisterCapability"
                    | "window/workDoneProgress/create"
                    | "window/showMessageRequest"
                    | "workspace/codeLens/refresh"
                    | "workspace/semanticTokens/refresh"
                    | "workspace/inlayHint/refresh"
                    | "workspace/diagnostic/refresh" => Ok(Value::Null),
                    "workspace/workspaceFolders" => {
                        Ok(json!([{"uri": self.root_uri, "name": "workspace"}]))
                    }
                    // The agent's tools own every file change.
                    "workspace/applyEdit" => Ok(
                        json!({"applied": false, "failureReason": "ShadowCode does not accept server edits"}),
                    ),
                    _ => Err(
                        json!({"code": -32601, "message": format!("{method} is not supported")}),
                    ),
                };
                let reply = match result {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
                };
                // Never block the reader on stdin: a server busy writing to
                // its stdout would otherwise deadlock against a long didOpen.
                let shared = self.clone();
                tokio::spawn(async move {
                    let _ = write_frame(&shared.writer, &reply).await;
                });
            }
            (Some(method), None) => match method {
                "textDocument/publishDiagnostics" => self.store(&message["params"]),
                "experimental/serverStatus" if message["params"]["quiescent"] == true => {
                    self.ready.store(true, Ordering::SeqCst);
                    self.changed.notify_waiters();
                }
                _ => {}
            },
            (None, Some(id)) => {
                let Some(id) = id.as_i64() else {
                    return;
                };
                let sender = self.pending.lock().ok().and_then(|mut p| p.remove(&id));
                if let Some(sender) = sender {
                    let outcome = match message.get("error") {
                        Some(error) if !error.is_null() => Err(error["message"]
                            .as_str()
                            .unwrap_or("language server error")
                            .to_owned()),
                        _ => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = sender.send(outcome);
                }
            }
            (None, None) => {}
        }
    }

    fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .map(|ring| {
                let bytes: Vec<u8> = ring.iter().copied().collect();
                String::from_utf8_lossy(&bytes).trim().to_owned()
            })
            .unwrap_or_default()
    }

    fn mark_dead(&self, reason: String) {
        self.alive.store(false, Ordering::SeqCst);
        if let Ok(mut exit) = self.exit.lock() {
            exit.get_or_insert(reason);
        }
        if let Ok(mut pending) = self.pending.lock() {
            for (_, sender) in pending.drain() {
                let _ = sender.send(Err("The language server stopped".into()));
            }
        }
        self.changed.notify_waiters();
    }
}

impl Client {
    /// Spawn, initialize, and return a ready-to-use client.
    pub async fn start(launch: &Launch, root: &Path) -> Result<Arc<Self>> {
        let mut command = tokio::process::Command::new(&launch.program);
        command
            .args(&launch.args)
            .current_dir(root)
            .env_clear()
            .envs(servers::environment(launch))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
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
        let mut child = command
            .spawn()
            .with_context(|| format!("Could not start {}", launch.program.display()))?;
        let stdin = child
            .stdin
            .take()
            .context("No stdin for the language server")?;
        let stdout = child
            .stdout
            .take()
            .context("No stdout for the language server")?;
        let stderr = child.stderr.take();
        let root_uri = uri_for(root);
        let shared = Arc::new(Shared {
            writer: tokio::sync::Mutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            diagnostics: Mutex::new(Diagnostics::default()),
            changed: Notify::new(),
            alive: AtomicBool::new(true),
            ready: AtomicBool::new(false),
            settings: launch.settings.clone(),
            root_uri: root_uri.clone(),
            stderr: Mutex::new(VecDeque::new()),
            exit: Mutex::new(None),
        });
        if let Some(mut stderr) = stderr {
            let shared = shared.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                while let Ok(n) = stderr.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut ring) = shared.stderr.lock() {
                        ring.extend(&buf[..n]);
                        while ring.len() > STDERR_BYTES {
                            ring.pop_front();
                        }
                    }
                }
            });
        }
        {
            let shared = shared.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let reason = loop {
                    match read_frame(&mut reader).await {
                        Ok(Some(message)) => shared.handle(message),
                        Ok(None) => break "The language server closed its output".to_owned(),
                        Err(error) => break format!("Language server protocol error: {error:#}"),
                    }
                };
                shared.mark_dead(reason);
            });
        }
        let client = Arc::new(Self {
            name: launch.name.clone(),
            root: root.to_owned(),
            launch: launch.clone(),
            started: Instant::now(),
            pid: child.id(),
            child: Mutex::new(Some(child)),
            shared,
            next_id: AtomicI64::new(1),
            documents: tokio::sync::Mutex::new(HashMap::new()),
            last_used: Mutex::new(Instant::now()),
            capabilities: Mutex::new(Value::Null),
        });
        let folder = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workspace".into());
        let init = client
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "clientInfo": {"name": "ShadowCode", "version": crate::VERSION},
                    "rootUri": root_uri,
                    "rootPath": root,
                    "workspaceFolders": [{"uri": root_uri, "name": folder}],
                    "initializationOptions": launch.init_options,
                    "capabilities": {
                        "workspace": {
                            "configuration": true,
                            "workspaceFolders": true,
                            "didChangeConfiguration": {"dynamicRegistration": false},
                        },
                        "textDocument": {
                            "synchronization": {"didSave": true, "dynamicRegistration": false},
                            "publishDiagnostics": {"relatedInformation": false, "versionSupport": true},
                            "definition": {"linkSupport": true},
                            "references": {},
                        },
                        "window": {"workDoneProgress": true},
                        "general": {"positionEncodings": ["utf-16"]},
                        "experimental": {"serverStatusNotification": true},
                    },
                }),
                INITIALIZE_TIMEOUT,
            )
            .await;
        let init = match init {
            Ok(init) => init,
            Err(error) => {
                client.kill();
                let tail = client.shared.stderr_tail();
                bail!(
                    "{} did not initialize: {error:#}{}",
                    launch.name,
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", crate::tools::truncate(&tail, 1500))
                    }
                );
            }
        };
        if let Ok(mut capabilities) = client.capabilities.lock() {
            *capabilities = init["capabilities"].clone();
        }
        client.notify("initialized", json!({})).await?;
        if !launch.settings.as_object().is_none_or(|s| s.is_empty()) {
            client
                .notify(
                    "workspace/didChangeConfiguration",
                    json!({"settings": launch.settings}),
                )
                .await?;
        }
        if launch.lang != servers::ServerLang::Rust {
            client.shared.ready.store(true, Ordering::SeqCst);
        }
        Ok(client)
    }

    pub fn alive(&self) -> bool {
        if !self.shared.alive.load(Ordering::SeqCst) {
            return false;
        }
        let exited = self
            .child
            .lock()
            .ok()
            .and_then(|mut child| child.as_mut().map(|c| c.try_wait()))
            .is_some_and(|status| !matches!(status, Ok(None)));
        if exited {
            self.shared.mark_dead(format!(
                "The language server exited. {}",
                crate::tools::truncate(&self.shared.stderr_tail(), 1000)
            ));
        }
        !exited
    }

    pub fn ready(&self) -> bool {
        self.shared.ready.load(Ordering::SeqCst)
    }

    pub fn exit_reason(&self) -> Option<String> {
        self.shared.exit.lock().ok().and_then(|e| e.clone())
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn touch(&self) {
        if let Ok(mut used) = self.last_used.lock() {
            *used = Instant::now();
        }
    }

    pub fn idle_for(&self) -> Duration {
        self.last_used
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    pub fn capabilities(&self) -> Value {
        self.capabilities
            .lock()
            .map(|c| c.clone())
            .unwrap_or(Value::Null)
    }

    pub async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        ensure!(
            self.shared.alive.load(Ordering::SeqCst),
            "The language server is not running"
        );
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.shared
            .pending
            .lock()
            .map_err(|_| anyhow!("Language server state lock poisoned"))?
            .insert(id, sender);
        write_frame(
            &self.shared.writer,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .await
        .inspect_err(|error| self.write_failed(error))?;
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(message))) => bail!("{method}: {message}"),
            Ok(Err(_)) => bail!("{method}: the language server stopped"),
            Err(_) => {
                if let Ok(mut pending) = self.shared.pending.lock() {
                    pending.remove(&id);
                }
                let _ = self.notify("$/cancelRequest", json!({"id": id})).await;
                bail!("{method} timed out after {}s", timeout.as_secs())
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        if !self.shared.alive.load(Ordering::SeqCst) {
            let reason = self.shared.exit.lock().ok().and_then(|exit| exit.clone());
            bail!(
                "{}",
                reason.unwrap_or_else(|| "The language server is not running".into())
            );
        }
        write_frame(
            &self.shared.writer,
            &json!({"jsonrpc": "2.0", "method": method, "params": params}),
        )
        .await
        .inspect_err(|error| self.write_failed(error))
    }

    /// A failed or unfinished write leaves the protocol stream unusable: the
    /// server is stopped, and the next use starts a fresh one.
    fn write_failed(&self, error: &anyhow::Error) {
        self.shared
            .mark_dead(format!("Could not write to the language server: {error:#}"));
        self.kill();
    }

    /// Current publish counter; diagnostics newer than it came after a change.
    pub fn seq(&self) -> u64 {
        self.shared.diagnostics.lock().map(|d| d.seq).unwrap_or(0)
    }

    pub fn published(&self, uri: &str) -> Option<Published> {
        self.shared
            .diagnostics
            .lock()
            .ok()?
            .by_uri
            .get(uri)
            .cloned()
    }

    pub fn all_published(&self) -> HashMap<String, Published> {
        self.shared
            .diagnostics
            .lock()
            .map(|d| d.by_uri.clone())
            .unwrap_or_default()
    }

    /// Version and digest of an open document.
    pub async fn document(&self, uri: &str) -> Option<(i64, String)> {
        self.documents
            .lock()
            .await
            .get(uri)
            .map(|d| (d.version, d.digest.clone()))
    }

    /// Open or update a document with full text. Returns its version and
    /// whether anything was sent. `saved` also sends didSave (the text is
    /// what is on disk).
    pub async fn sync(
        &self,
        uri: &str,
        language_id: &str,
        text: &str,
        saved: bool,
    ) -> Result<(i64, bool)> {
        self.touch();
        let digest = digest(text);
        let mut documents = self.documents.lock().await;
        let (version, sent) = match documents.get_mut(uri) {
            Some(doc) if doc.digest == digest => {
                doc.used = Instant::now();
                (doc.version, false)
            }
            Some(doc) => {
                doc.version += 1;
                doc.digest = digest;
                doc.used = Instant::now();
                let version = doc.version;
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": uri, "version": version},
                        "contentChanges": [{"text": text}],
                    }),
                )
                .await?;
                (version, true)
            }
            None => {
                if documents.len() >= MAX_OPEN_DOCUMENTS {
                    if let Some(oldest) = documents
                        .iter()
                        .min_by_key(|(_, d)| d.used)
                        .map(|(k, _)| k.clone())
                    {
                        documents.remove(&oldest);
                        self.notify(
                            "textDocument/didClose",
                            json!({"textDocument": {"uri": oldest}}),
                        )
                        .await?;
                        if let Ok(mut diagnostics) = self.shared.diagnostics.lock() {
                            diagnostics.by_uri.remove(&oldest);
                        }
                    }
                }
                documents.insert(
                    uri.to_owned(),
                    Document {
                        version: 1,
                        digest,
                        used: Instant::now(),
                    },
                );
                self.notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {"uri": uri, "languageId": language_id, "version": 1, "text": text},
                    }),
                )
                .await?;
                (1, true)
            }
        };
        if saved && sent {
            self.notify(
                "textDocument/didSave",
                json!({"textDocument": {"uri": uri}}),
            )
            .await?;
        }
        Ok((version, sent))
    }

    /// Wait for diagnostics on `uri` published after `after_seq` (and not for
    /// an older document version), then for `quiet` without a newer publish.
    /// None when the deadline passes first or the server stops.
    pub async fn wait_for(
        &self,
        uri: &str,
        after_seq: u64,
        version: Option<i64>,
        deadline: Instant,
        quiet: Duration,
    ) -> Option<Published> {
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.shared.alive.load(Ordering::SeqCst) {
                return None;
            }
            let now = Instant::now();
            let fresh = self.published(uri).filter(|p| {
                p.seq > after_seq
                    && match (p.version, version) {
                        (Some(published), Some(wanted)) => published >= wanted,
                        _ => true,
                    }
            });
            let wake = match &fresh {
                Some(p) if p.at.elapsed() >= quiet || now >= deadline => return fresh,
                Some(p) => (p.at + quiet).min(deadline),
                None if now >= deadline => return None,
                None => deadline,
            };
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep_until(wake.into()) => {}
            }
        }
    }

    /// Wait until the server has loaded the project (or `deadline`).
    pub async fn wait_ready(&self, deadline: Instant) -> bool {
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.ready() {
                return true;
            }
            if !self.shared.alive.load(Ordering::SeqCst) || Instant::now() >= deadline {
                return false;
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep_until(deadline.into()) => {}
            }
        }
    }

    /// True when the server answers `textDocument/diagnostic` (LSP 3.17 pull
    /// diagnostics). rust-analyzer pushes only when a file's diagnostics
    /// change, so after an edit that changes nothing it would stay silent;
    /// asking gives a definite answer.
    pub fn pulls(&self) -> bool {
        self.capabilities()["diagnosticProvider"].is_object()
    }

    /// Ask for a document's diagnostics, retrying when the server reports the
    /// content changed or it cancelled the request itself.
    pub async fn pull(
        &self,
        uri: &str,
        version: Option<i64>,
        deadline: Instant,
    ) -> Option<Published> {
        for _ in 0..4 {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            match self
                .request(
                    "textDocument/diagnostic",
                    json!({"textDocument": {"uri": uri}}),
                    remaining,
                )
                .await
            {
                Ok(result) => {
                    let mut items = result["items"].as_array().cloned().unwrap_or_default();
                    items.truncate(MAX_DIAGNOSTICS_PER_FILE);
                    self.shared
                        .store(&json!({"uri": uri, "version": version, "diagnostics": items}));
                    return self.published(uri);
                }
                Err(error) => {
                    let text = format!("{error:#}");
                    if !(text.contains("modified") || text.contains("cancel")) {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        None
    }

    /// Diagnostics for `uri` after a change: pulled when the server offers
    /// it, else the next push (see `wait_for`).
    pub async fn settle(
        &self,
        uri: &str,
        after_seq: u64,
        version: Option<i64>,
        deadline: Instant,
        quiet: Duration,
    ) -> Option<Published> {
        if self.pulls() {
            return self.pull(uri, version, deadline).await;
        }
        self.wait_for(uri, after_seq, version, deadline, quiet)
            .await
    }

    /// Ask the server to exit, then make sure it does.
    pub async fn shutdown(&self) {
        if self.shared.alive.load(Ordering::SeqCst) {
            let _ = self
                .request("shutdown", Value::Null, Duration::from_secs(2))
                .await;
            let _ = self.notify("exit", Value::Null).await;
            for _ in 0..10 {
                if !self.alive() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        self.kill();
        self.shared.mark_dead("Stopped by ShadowCode".into());
    }

    fn kill(&self) {
        if let Ok(mut child) = self.child.lock() {
            if let Some(child) = child.as_mut() {
                #[cfg(unix)]
                if let Some(pid) = child.id() {
                    // The server runs in its own process group; take helpers
                    // (tsserver, cargo metadata) with it.
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                let _ = child.start_kill();
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip_with_escapes() {
        let path = Path::new("/tmp/a b/ü#1.rs");
        let uri = uri_for(path);
        assert_eq!(uri, "file:///tmp/a%20b/%C3%BC%231.rs");
        assert_eq!(path_for(&uri).unwrap(), path);
    }

    #[test]
    fn columns_convert_between_chars_and_utf16() {
        let line = "a😀b";
        assert_eq!(utf16_column(line, 2), 3);
        assert_eq!(char_column(line, 3), 2);
        assert_eq!(char_column(line, 99), 3);
    }
}

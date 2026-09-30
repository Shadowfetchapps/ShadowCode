//! What a new task worktree needs to run: files copied from the project
//! (such as `.env`, which Git does not carry), setup commands (`npm ci`),
//! teardown commands run before the worktree is removed, and a port of its
//! own so two tasks' dev servers don't collide.
//!
//! The commands are the user's own, saved per project (never suggested ones
//! until the user saves them); they run in the worktree as the user, with
//! `PORT` and `SHADOWCODE_PORT` set. Each task's port is free on this
//! computer and not used by another open worktree task of the project, and
//! the task's shells and subscription CLIs get it too (`keys::TASK_ENV`).
use crate::store::{keys, Store};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

/// Longest a setup command may run, and a teardown command.
const SETUP_TIMEOUT: Duration = Duration::from_secs(600);
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(120);
/// Largest file copied into a worktree.
const MAX_COPY_BYTES: u64 = 10 * 1024 * 1024;
/// Output kept from each stream of a command.
const OUTPUT_KEPT: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Setup {
    /// Project files copied into each new worktree (relative paths).
    pub copy: Vec<String>,
    /// Commands run in the new worktree before its first turn.
    pub setup: Vec<String>,
    /// Commands run in the worktree before it is removed.
    pub teardown: Vec<String>,
    /// The ports tasks get, first and last.
    pub port_start: u16,
    pub port_end: u16,
}
impl Default for Setup {
    fn default() -> Self {
        Self {
            copy: Vec::new(),
            setup: Vec::new(),
            teardown: Vec::new(),
            port_start: 3100,
            port_end: 3999,
        }
    }
}

pub fn load(store: &Store, workspace: &Path) -> Setup {
    store
        .native_meta(&keys::worktree_setup(workspace))
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(store: &Store, workspace: &Path, setup: Setup) -> Result<Setup> {
    ensure!(setup.copy.len() <= 20, "Copy at most 20 files");
    ensure!(
        setup.setup.len() <= 10 && setup.teardown.len() <= 10,
        "Use at most 10 commands"
    );
    for path in &setup.copy {
        ensure!(
            !path.is_empty()
                && !path.starts_with('/')
                && !path.split('/').any(|part| part == ".." || part == ".git"),
            "Copy files inside the project: {path}"
        );
    }
    for command in setup.setup.iter().chain(&setup.teardown) {
        ensure!(
            !command.trim().is_empty() && command.len() <= 500 && !command.contains('\0'),
            "Commands must be one line of at most 500 characters"
        );
    }
    ensure!(
        setup.port_start >= 1024 && setup.port_start <= setup.port_end,
        "Choose a port range from 1024 up"
    );
    store.set_native_meta(
        &keys::worktree_setup(workspace),
        &serde_json::to_string(&setup)?,
    )?;
    Ok(setup)
}

/// Settings to offer for a project, from its files (the user saves them).
pub fn suggest(workspace: &Path) -> Setup {
    let has = |name: &str| workspace.join(name).is_file();
    let mut setup = Setup::default();
    if has("pnpm-lock.yaml") {
        setup.setup.push("pnpm install --frozen-lockfile".into());
    } else if has("yarn.lock") {
        setup.setup.push("yarn install --frozen-lockfile".into());
    } else if has("package-lock.json") {
        setup.setup.push("npm ci".into());
    } else if has("bun.lockb") || has("bun.lock") {
        setup.setup.push("bun install --frozen-lockfile".into());
    }
    if has("uv.lock") {
        setup.setup.push("uv sync".into());
    } else if has("poetry.lock") {
        setup.setup.push("poetry install".into());
    }
    for env in [".env", ".env.local", ".env.development"] {
        if has(env) {
            setup.copy.push(env.into());
        }
    }
    setup
}

/// A port in the range that is free now and not in `taken`.
pub fn free_port(setup: &Setup, taken: &[u16]) -> Option<u16> {
    (setup.port_start..=setup.port_end)
        .filter(|port| !taken.contains(port))
        .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
}

/// The environment a task's commands get.
pub fn env_for(port: Option<u16>) -> BTreeMap<String, String> {
    port.map(|port| {
        BTreeMap::from([
            ("PORT".to_owned(), port.to_string()),
            ("SHADOWCODE_PORT".to_owned(), port.to_string()),
        ])
    })
    .unwrap_or_default()
}

/// The task environment saved for a conversation.
pub fn session_env(store: &Store, session_id: &str) -> BTreeMap<String, String> {
    store
        .session_meta(session_id, keys::TASK_ENV)
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// What a command's output keeps while it runs (its end is what matters).
struct Tail {
    text: Arc<Mutex<Vec<u8>>>,
    reader: Option<tokio::task::JoinHandle<()>>,
}
impl Tail {
    fn read(stream: Option<impl AsyncRead + Unpin + Send + 'static>) -> Self {
        let text = Arc::new(Mutex::new(Vec::new()));
        let reader = stream.map(|mut stream| {
            let text = text.clone();
            tokio::spawn(async move {
                let mut chunk = [0_u8; 8192];
                loop {
                    let count = match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(count) => count,
                    };
                    if let Ok(mut text) = text.lock() {
                        text.extend_from_slice(&chunk[..count]);
                        if text.len() > 2 * OUTPUT_KEPT {
                            let cut = text.len() - OUTPUT_KEPT;
                            text.drain(..cut);
                        }
                    }
                }
            })
        });
        Self { text, reader }
    }
    /// What arrived by `until`. A process the command left running in the
    /// background may hold the output open: its reader keeps draining it, so
    /// that process is not stopped by a closed pipe.
    async fn until(mut self, until: tokio::time::Instant) -> Vec<u8> {
        if let Some(reader) = self.reader.as_mut() {
            let _ = tokio::time::timeout_at(until, reader).await;
        }
        self.text
            .lock()
            .map(|text| text.clone())
            .unwrap_or_default()
    }
}

/// Run one command with `sh -c` in its own process group. It is over when
/// the shell exits, even if something it started in the background still
/// runs; after `timeout`, or when `cancel` fires (ShadowCode is closing),
/// the whole group is killed. `{command, ok, exit_code, seconds, output}`,
/// plus `stopped` ("timeout", "cancelled", "signal" with `signal`, or
/// "not_started") when it did not exit by itself.
async fn run_command(
    dir: &Path,
    command: &str,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Value {
    let started = Instant::now();
    let seconds = || (started.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    if cancel.is_cancelled() {
        return json!({"command": command, "ok": false, "stopped": "cancelled", "seconds": 0.0, "output": "Not started: ShadowCode is closing"});
    }
    let mut process = tokio::process::Command::new("sh");
    process
        .args(["-c", command])
        .current_dir(dir)
        .envs(env)
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    process.process_group(0);
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return json!({"command": command, "ok": false, "stopped": "not_started", "seconds": seconds(), "output": format!("Could not start: {error}")})
        }
    };
    let stdout = Tail::read(child.stdout.take());
    let stderr = Tail::read(child.stderr.take());
    let waited = tokio::select! {
        status = child.wait() => Ok(status),
        () = tokio::time::sleep(timeout) => Err("timeout"),
        () = cancel.cancelled() => Err("cancelled"),
    };
    if waited.is_err() {
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            // The command leads its own group: this stops what it started too.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = child.kill().await;
    }
    let grace = tokio::time::Instant::now() + Duration::from_secs(1);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&stdout.until(grace).await),
        String::from_utf8_lossy(&stderr.until(grace).await)
    );
    let mut tail: String = text
        .chars()
        .rev()
        .take(2000)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut result = json!({"command": command, "ok": false, "seconds": seconds()});
    match waited {
        Ok(Ok(status)) => {
            result["ok"] = json!(status.success());
            result["exit_code"] = json!(status.code());
            #[cfg(unix)]
            if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
                result["stopped"] = json!("signal");
                result["signal"] = json!(signal);
            }
        }
        Ok(Err(error)) => tail.push_str(&format!("\nCould not wait for it: {error}")),
        Err(stopped) => {
            result["stopped"] = json!(stopped);
            tail.push_str(&if stopped == "timeout" {
                format!("\nStopped after {} seconds", timeout.as_secs())
            } else {
                "\nStopped: ShadowCode is closing".to_owned()
            });
        }
    }
    result["output"] = json!(crate::redaction::redact_text(tail.trim_start()).text);
    result
}

/// Copy one project file into the worktree. Neither side may lead out of its
/// folder: the project file must resolve inside the project (and not in
/// `.git`), and the worktree's folders on the way must be real folders, not
/// symlinks, since the branch it starts from may commit any. The copy is
/// written beside its place and renamed over it, never through a symlink.
fn copy_file(source: &Path, worktree: &Path, path: &str) -> std::result::Result<(), String> {
    let refused = || "outside the project, in .git or through a symlink".to_owned();
    let from = source.join(path);
    let copyable =
        fs::symlink_metadata(&from).is_ok_and(|m| m.is_file() && m.len() <= MAX_COPY_BYTES);
    if !copyable {
        return Err("missing, not a regular file, or over 10 MB".into());
    }
    let (Ok(source), Ok(worktree), Ok(real)) = (
        source.canonicalize(),
        worktree.canonicalize(),
        from.canonicalize(),
    ) else {
        return Err(refused());
    };
    match real.strip_prefix(&source) {
        Ok(inside) if !inside.components().any(|part| part.as_os_str() == ".git") => {}
        _ => return Err(refused()),
    }
    let relative = Path::new(path);
    let name = relative.file_name().ok_or_else(refused)?;
    let mut folder = worktree;
    for part in relative.parent().into_iter().flat_map(Path::components) {
        match part {
            Component::CurDir => continue,
            Component::Normal(part) => folder.push(part),
            _ => return Err(refused()),
        }
        match fs::symlink_metadata(&folder) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(refused()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&folder).map_err(|error| error.to_string())?
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    let to = folder.join(name);
    if fs::symlink_metadata(&to).is_ok_and(|meta| !meta.is_file()) {
        return Err(refused());
    }
    let written = (|| -> std::io::Result<()> {
        let mut copy = tempfile::NamedTempFile::new_in(&folder)?;
        std::io::copy(&mut fs::File::open(&real)?, copy.as_file_mut())?;
        copy.as_file()
            .set_permissions(fs::metadata(&real)?.permissions())?;
        copy.persist(&to).map_err(|error| error.error)?;
        Ok(())
    })();
    written.map_err(|error| error.to_string())
}

/// Prepare a new worktree: copy the chosen files, then run the setup
/// commands (stopping at the first failure, or when `cancel` fires).
/// `{ok, copied, skipped, commands}`.
pub async fn run_setup(
    setup: &Setup,
    source: &Path,
    worktree: &Path,
    port: Option<u16>,
    cancel: &CancellationToken,
) -> Value {
    let mut copied = Vec::new();
    let mut skipped = Vec::new();
    for path in &setup.copy {
        match copy_file(source, worktree, path) {
            Ok(()) => copied.push(path.clone()),
            Err(reason) => skipped.push(json!({"path": path, "reason": reason})),
        }
    }
    let env = env_for(port);
    let mut commands = Vec::new();
    let mut ok = true;
    for command in &setup.setup {
        let result = run_command(worktree, command, &env, SETUP_TIMEOUT, cancel).await;
        ok = result["ok"] == true;
        commands.push(result);
        if !ok {
            break;
        }
    }
    json!({"ok": ok, "copied": copied, "skipped": skipped, "commands": commands, "port": port})
}

/// Before a worktree is removed: its teardown commands, best effort. They
/// are cleanup, so closing ShadowCode waits for them rather than stop them.
pub async fn run_teardown(setup: &Setup, worktree: &Path, port: Option<u16>) -> Vec<Value> {
    let env = env_for(port);
    let never = CancellationToken::new();
    let mut results = Vec::new();
    for command in &setup.teardown {
        results.push(run_command(worktree, command, &env, TEARDOWN_TIMEOUT, &never).await);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never() -> CancellationToken {
        CancellationToken::new()
    }

    #[test]
    fn suggestions_follow_the_lockfiles_and_env_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package-lock.json"), "{}").unwrap();
        std::fs::write(dir.path().join(".env"), "A=1").unwrap();
        let setup = suggest(dir.path());
        assert_eq!(setup.setup, ["npm ci"]);
        assert_eq!(setup.copy, [".env"]);
    }

    #[test]
    fn ports_skip_taken_and_busy_ones() {
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = busy.local_addr().unwrap().port();
        let setup = Setup {
            port_start: port,
            port_end: port.saturating_add(3),
            ..Setup::default()
        };
        let chosen = free_port(&setup, &[port.saturating_add(1)]).unwrap();
        assert!(chosen != port && chosen != port + 1, "{chosen}");
        assert_eq!(env_for(Some(4100))["PORT"], "4100");
    }

    #[tokio::test]
    async fn setup_copies_files_and_stops_at_the_first_failing_command() {
        let source = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join(".env"), "TOKEN=local").unwrap();
        let setup = Setup {
            copy: vec![".env".into(), "missing.txt".into()],
            setup: vec![
                "printf \"$PORT\" > port.txt".into(),
                "exit 7".into(),
                "touch never".into(),
            ],
            ..Setup::default()
        };
        let outcome = run_setup(&setup, source.path(), worktree.path(), Some(4321), &never()).await;
        assert_eq!(outcome["ok"], false);
        assert_eq!(outcome["copied"], json!([".env"]));
        assert_eq!(outcome["skipped"][0]["path"], "missing.txt");
        assert_eq!(outcome["commands"].as_array().unwrap().len(), 2);
        assert_eq!(outcome["commands"][1]["exit_code"], 7);
        assert_eq!(
            std::fs::read_to_string(worktree.path().join("port.txt")).unwrap(),
            "4321"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.path().join(".env")).unwrap(),
            "TOKEN=local"
        );
        assert!(!worktree.path().join("never").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn copies_never_follow_a_symlink_out_of_the_project_or_the_worktree() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let (source, worktree, outside) = (
            root.path().join("s"),
            root.path().join("w"),
            root.path().join("outside"),
        );
        for dir in [&source, &worktree, &outside] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(source.join(".env"), "TOKEN=local").unwrap();
        fs::create_dir_all(source.join("apps/web")).unwrap();
        fs::write(source.join("apps/web/.env"), "WEB=1").unwrap();
        fs::write(outside.join("keys"), "outside bytes").unwrap();
        fs::write(outside.join(".env"), "OUTSIDE=1").unwrap();
        // The branch the worktree starts from committed `.env` as a symlink
        // to a file outside, and `apps` as a symlink to a folder outside.
        symlink(outside.join("keys"), worktree.join(".env")).unwrap();
        symlink(&outside, worktree.join("apps")).unwrap();
        // The project reaches outside through a symlinked folder.
        symlink(&outside, source.join("shared")).unwrap();
        let setup = Setup {
            copy: vec![".env".into(), "apps/web/.env".into(), "shared/.env".into()],
            ..Setup::default()
        };
        let outcome = run_setup(&setup, &source, &worktree, None, &never()).await;
        assert_eq!(outcome["copied"], json!([]), "{outcome}");
        assert_eq!(outcome["skipped"].as_array().unwrap().len(), 3);
        assert_eq!(
            fs::read_to_string(outside.join("keys")).unwrap(),
            "outside bytes"
        );
        assert!(!outside.join("web").exists());
        assert!(fs::symlink_metadata(worktree.join(".env"))
            .unwrap()
            .is_symlink());
        // A real folder in a fresh worktree is created and copied into.
        let fresh = root.path().join("fresh");
        fs::create_dir(&fresh).unwrap();
        let outcome = run_setup(&setup, &source, &fresh, None, &never()).await;
        assert_eq!(
            outcome["copied"],
            json!([".env", "apps/web/.env"]),
            "{outcome}"
        );
        assert_eq!(
            fs::read_to_string(fresh.join("apps/web/.env")).unwrap(),
            "WEB=1"
        );
        assert!(!fresh.join("shared").exists());
    }

    #[tokio::test]
    async fn a_command_that_leaves_a_process_running_is_done_when_its_shell_exits() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let setup = Setup {
            setup: vec!["sleep 8 &".into(), "echo second".into()],
            ..Setup::default()
        };
        let outcome = run_setup(&setup, dir.path(), dir.path(), None, &never()).await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(outcome["ok"], true, "{outcome}");
        assert_eq!(outcome["commands"][1]["output"], "second\n");
    }

    #[tokio::test]
    async fn a_timeout_stops_everything_the_command_started_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let env = BTreeMap::new();
        let late = dir.path().join("late");
        let result = run_command(
            dir.path(),
            "echo begun; (sleep 2; touch late) & sleep 30",
            &env,
            Duration::from_secs(1),
            &never(),
        )
        .await;
        assert_eq!(result["ok"], false);
        assert_eq!(result["stopped"], "timeout");
        assert!(result["exit_code"].is_null());
        let output = result["output"].as_str().unwrap();
        assert!(
            output.starts_with("begun") && output.ends_with("Stopped after 1 seconds"),
            "{output}"
        );
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!late.exists(), "the background part outlived the timeout");

        let killed = run_command(
            dir.path(),
            "kill -9 $$",
            &env,
            Duration::from_secs(10),
            &never(),
        )
        .await;
        assert_eq!(killed["stopped"], "signal");
        assert_eq!(killed["signal"], 9);
        assert!(killed["exit_code"].is_null());
        let missing = run_command(
            &dir.path().join("gone"),
            "true",
            &env,
            Duration::from_secs(10),
            &never(),
        )
        .await;
        assert_eq!(missing["stopped"], "not_started");
    }

    #[tokio::test]
    async fn closing_shadowcode_stops_the_setup_and_everything_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let late = dir.path().join("late");
        let setup = Setup {
            setup: vec![
                "echo begun; (sleep 2; touch late) & sleep 30".into(),
                "touch next".into(),
            ],
            ..Setup::default()
        };
        let closing = CancellationToken::new();
        let cancel = closing.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            cancel.cancel();
        });
        let started = Instant::now();
        let outcome = run_setup(&setup, dir.path(), dir.path(), None, &closing).await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(outcome["ok"], false, "{outcome}");
        let commands = outcome["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 1, "{outcome}");
        assert_eq!(commands[0]["stopped"], "cancelled");
        let output = commands[0]["output"].as_str().unwrap();
        assert!(
            output.starts_with("begun") && output.ends_with("Stopped: ShadowCode is closing"),
            "{output}"
        );
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!late.exists(), "the background part outlived the setup");
        assert!(!dir.path().join("next").exists());

        // Once closing, nothing more starts.
        let skipped = run_command(
            dir.path(),
            "touch next",
            &BTreeMap::new(),
            Duration::from_secs(10),
            &closing,
        )
        .await;
        assert_eq!(skipped["stopped"], "cancelled");
        assert!(!dir.path().join("next").exists());
    }
}

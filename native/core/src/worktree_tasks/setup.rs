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
    path::Path,
    time::{Duration, Instant},
};

/// Longest a setup command may run, and a teardown command.
const SETUP_TIMEOUT: Duration = Duration::from_secs(600);
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(120);
/// Largest file copied into a worktree.
const MAX_COPY_BYTES: u64 = 10 * 1024 * 1024;

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

async fn run_command(
    dir: &Path,
    command: &str,
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> Value {
    let started = Instant::now();
    let mut process = tokio::process::Command::new("sh");
    process
        .args(["-c", command])
        .current_dir(dir)
        .envs(env)
        .env("CI", "1")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let result = tokio::time::timeout(timeout, process.output()).await;
    let seconds = (started.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    match result {
        Ok(Ok(output)) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let tail: String = text
                .chars()
                .rev()
                .take(2000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            json!({
                "command": command,
                "ok": output.status.success(),
                "exit_code": output.status.code(),
                "seconds": seconds,
                "output": crate::redaction::redact_text(&tail).text,
            })
        }
        Ok(Err(error)) => {
            json!({"command": command, "ok": false, "seconds": seconds, "output": format!("Could not start: {error}")})
        }
        Err(_) => {
            json!({"command": command, "ok": false, "seconds": seconds, "output": format!("Stopped after {} seconds", timeout.as_secs())})
        }
    }
}

/// Prepare a new worktree: copy the chosen files, then run the setup
/// commands (stopping at the first failure). `{ok, copied, skipped,
/// commands}`.
pub async fn run_setup(setup: &Setup, source: &Path, worktree: &Path, port: Option<u16>) -> Value {
    let mut copied = Vec::new();
    let mut skipped = Vec::new();
    for path in &setup.copy {
        let from = source.join(path);
        let to = worktree.join(path);
        let copyable = std::fs::symlink_metadata(&from)
            .is_ok_and(|m| m.is_file() && m.len() <= MAX_COPY_BYTES);
        if !copyable {
            skipped.push(
                json!({"path": path, "reason": "missing, not a regular file, or over 10 MB"}),
            );
            continue;
        }
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::copy(&from, &to) {
            Ok(_) => copied.push(path.clone()),
            Err(error) => skipped.push(json!({"path": path, "reason": error.to_string()})),
        }
    }
    let env = env_for(port);
    let mut commands = Vec::new();
    let mut ok = true;
    for command in &setup.setup {
        let result = run_command(worktree, command, &env, SETUP_TIMEOUT).await;
        ok = result["ok"] == true;
        commands.push(result);
        if !ok {
            break;
        }
    }
    json!({"ok": ok, "copied": copied, "skipped": skipped, "commands": commands, "port": port})
}

/// Before a worktree is removed: its teardown commands, best effort.
pub async fn run_teardown(setup: &Setup, worktree: &Path, port: Option<u16>) -> Vec<Value> {
    let env = env_for(port);
    let mut results = Vec::new();
    for command in &setup.teardown {
        results.push(run_command(worktree, command, &env, TEARDOWN_TIMEOUT).await);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let outcome = run_setup(&setup, source.path(), worktree.path(), Some(4321)).await;
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
}

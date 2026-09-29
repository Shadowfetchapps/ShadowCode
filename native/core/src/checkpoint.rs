use crate::{
    store::Store,
    workspace::{hash, Snapshot, Workspace},
};
use anyhow::{ensure, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub mod capture;
pub mod turn_edits;

/// Checkpoints of the whole project around shell commands and vendor CLI
/// turns (`checkpoints` in the configuration).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CheckpointConfig {
    /// Capture before every native `exec` command that can write.
    pub shell: bool,
    /// Capture before every subscription CLI turn (Codex, Claude, ...).
    pub vendor: bool,
    /// Git checkpoint refs kept per repository under refs/shadowcode/checkpoints.
    pub keep: usize,
    /// Folders without Git: at most this many files are copied...
    pub max_copy_files: usize,
    /// ...totalling at most this many bytes; larger folders are not covered.
    pub max_copy_bytes: u64,
}
impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            shell: true,
            vendor: true,
            keep: 200,
            max_copy_files: 5_000,
            max_copy_bytes: 64 * 1024 * 1024,
        }
    }
}
impl CheckpointConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=10_000).contains(&self.keep),
            "checkpoints.keep must be between 1 and 10000"
        );
        ensure!(
            self.max_copy_files <= 200_000,
            "checkpoints.max_copy_files must be at most 200000"
        );
        ensure!(
            self.max_copy_bytes <= 1024 * 1024 * 1024,
            "checkpoints.max_copy_bytes must be at most 1 GiB"
        );
        Ok(())
    }
}

/// Record a change that already happened (a shell command or a vendor CLI
/// wrote the file): the content before it, taken from a workspace checkpoint,
/// and the hash of what is on disk now. The first recorded original of a path
/// in a task is kept, so rewind returns to the state before the task.
pub fn record_external(
    store: &Store,
    workspace: &Workspace,
    task: &str,
    path: &str,
    before: Option<&[u8]>,
    before_mode: Option<u32>,
    current_hash: Option<&str>,
) -> Result<()> {
    let path = workspace.relative(path)?.to_string_lossy().into_owned();
    let before_hash = before.map(hash).unwrap_or_else(|| "missing".into());
    store.execute("INSERT INTO file_changes(task_id,workspace,path,before_bytes,before_mode,after_hash,observed_hash) VALUES(?,?,?,?,?,?,?)
        ON CONFLICT(task_id,workspace,path) DO UPDATE SET after_hash=excluded.after_hash,observed_hash=excluded.observed_hash,restored=0",
        params![task,workspace.path.to_string_lossy(),path,before,before_mode,current_hash.unwrap_or("missing"),before_hash])?;
    Ok(())
}

/// Record the original once and the intended current content before each write.
/// A crash before the write is a no-op; a crash afterwards remains rewindable.
pub fn record(
    store: &Store,
    workspace: &Workspace,
    task: &str,
    path: &str,
    before: &Snapshot,
    after: Option<&[u8]>,
) -> Result<()> {
    let path = workspace.relative(path)?.to_string_lossy().into_owned();
    let after_hash = after.map(hash).unwrap_or_else(|| "missing".into());
    store.execute("INSERT INTO file_changes(task_id,workspace,path,before_bytes,before_mode,after_hash,observed_hash) VALUES(?,?,?,?,?,?,?)
        ON CONFLICT(task_id,workspace,path) DO UPDATE SET after_hash=excluded.after_hash,observed_hash=excluded.observed_hash,restored=0",
        params![task,workspace.path.to_string_lossy(),path,before.bytes,before.mode,after_hash,before.hash.as_deref().unwrap_or("missing")])?;
    Ok(())
}

pub fn summary(store: &Store, workspace: &Workspace, task: &str) -> Result<Value> {
    let rows = store.query(
        "SELECT path,restored FROM file_changes WHERE task_id=? AND workspace=? ORDER BY id",
        params![task, workspace.path.to_string_lossy()],
    )?;
    // What a rewind does now: files it restores, files it keeps because you
    // saved them during a subscription turn, and changes the agent did not
    // report (see `turn_edits`).
    let plan = turn_edits::plan(store, workspace, task)?;
    Ok(
        json!({"task_id":task,"workspace":workspace.path,"changes":rows.len(),"paths":rows.iter().map(|r|r["path"].clone()).collect::<Vec<_>>(),"restored":!rows.is_empty() && rows.iter().all(|r|r["restored"]==1),
            "rewind_paths":plan.rewind,"kept":plan.kept,"unreported":plan.unreported}),
    )
}

/// Rewind `task`'s files. Files you saved in the editor during a
/// subscription turn are kept (see `turn_edits`).
pub fn restore(store: &Store, workspace: &Workspace, task: &str) -> Result<Vec<String>> {
    restore_with(store, workspace, task, false)
}

/// [`restore`]; `include_user_edits` also rewinds files you saved during the
/// turn that the agent edited too.
pub fn restore_with(
    store: &Store,
    workspace: &Workspace,
    task: &str,
    include_user_edits: bool,
) -> Result<Vec<String>> {
    let keep = if include_user_edits {
        Default::default()
    } else {
        turn_edits::plan(store, workspace, task)?.keep_set()
    };
    struct Change {
        id: i64,
        path: String,
        before: Option<Vec<u8>>,
        mode: Option<u32>,
        after: String,
        current: Snapshot,
    }
    let mut changes = Vec::new();
    {
        let db = store.lock()?;
        let mut statement=db.prepare("SELECT id,path,before_bytes,before_mode,after_hash,observed_hash FROM file_changes WHERE task_id=? AND workspace=? AND restored=0 ORDER BY id DESC")?;
        let rows = statement.query_map(params![task, workspace.path.to_string_lossy()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?,
                r.get::<_, Option<u32>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        for row in rows {
            let (id, path, before, mode, after, observed) = row?;
            if keep.contains(&path) {
                continue;
            }
            let current = workspace.snapshot(&path)?;
            let before_hash = before
                .as_deref()
                .map(hash)
                .unwrap_or_else(|| "missing".into());
            let current_hash = current.hash.as_deref().unwrap_or("missing");
            ensure!(
                after.as_deref() == Some(current_hash)
                    || observed.as_deref() == Some(current_hash)
                    || before_hash == current_hash,
                "Cannot rewind: {path} was changed after this task. No files were restored."
            );
            changes.push(Change {
                id,
                path,
                before,
                mode,
                after: current_hash.to_owned(),
                current,
            });
        }
    }
    let mut restored = Vec::new();
    for change in changes {
        let current_hash = change.current.hash.as_deref().unwrap_or("missing");
        let before_hash = change
            .before
            .as_deref()
            .map(hash)
            .unwrap_or_else(|| "missing".into());
        if before_hash != current_hash {
            ensure!(change.after == current_hash, "Checkpoint mismatch");
            match &change.before {
                Some(bytes) => {
                    workspace.write(&change.path, bytes, Some(current_hash))?;
                    if let Some(mode) = change.mode {
                        workspace.set_mode(&change.path, mode)?;
                    }
                }
                None => workspace.delete(&change.path, Some(current_hash))?,
            }
        } else if let (Some(mode), Some(current)) = (change.mode, change.current.mode) {
            // Same content, different permissions (a `chmod` by a command or
            // a vendor): put the earlier permissions back.
            if current != mode & 0o777 {
                workspace.set_mode(&change.path, mode)?;
            }
        }
        store.execute("UPDATE file_changes SET restored=1 WHERE id=?", [change.id])?;
        restored.push(change.path);
    }
    Ok(restored)
}

/// After a finished task's files were restored, make the session's message
/// tape say so, so the next turn does not believe the undone edits are still
/// on disk. System messages are not carried between turns, so the note is
/// appended to the last assistant message (or added as one), which keeps
/// user/assistant alternation intact for strict chat templates.
pub fn note_restore_in_tape(store: &Store, session_id: &str, paths: &[String]) -> Result<()> {
    note_in_tape(store, session_id, &restore_note(paths))
}

/// Append a process note to the session's message tape (see
/// `note_restore_in_tape`).
pub fn note_in_tape(store: &Store, session_id: &str, note: &str) -> Result<()> {
    let rows = store.query(
        "SELECT id FROM desktop_jobs WHERE json_extract(payload,'$.session_id')=? AND EXISTS(SELECT 1 FROM job_messages WHERE job_id=desktop_jobs.id) ORDER BY rowid DESC LIMIT 1",
        params![session_id],
    )?;
    let Some(job_id) = rows.first().and_then(|r| r["id"].as_str()) else {
        return Ok(());
    };
    let mut messages = store.messages(job_id)?;
    match messages.last_mut() {
        Some(last)
            if last["role"] == "assistant"
                && last
                    .get("tool_calls")
                    .is_none_or(|calls| calls.as_array().is_none_or(Vec::is_empty)) =>
        {
            let content = last["content"].as_str().unwrap_or("").to_owned();
            last["content"] = json!(format!("{content}\n\n{note}").trim().to_owned());
        }
        _ => messages.push(json!({"role":"assistant","content":note})),
    }
    store.save_messages(job_id, &messages)
}

pub fn restore_note(paths: &[String]) -> String {
    format!(
        "[Process note from ShadowCode, not a user request: the file checkpoint was restored afterwards for {} path(s): {}. Those edits are no longer on disk; re-read the files before editing.]",
        paths.len(),
        paths.join(", ")
    )
}

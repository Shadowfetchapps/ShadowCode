//! Worktree tasks ("Run in new worktree"): a new conversation whose turns
//! run in its own managed Git worktree, so it can work while another task
//! runs in the project's main checkout. The engine allows one active task per
//! checkout (folder); a worktree is a different folder, so both run at once.
//!
//! The worktree starts from the same state Compare uses: HEAD plus the
//! project's uncommitted, non-ignored work, captured without touching the
//! project's index or working tree. When the task is done the user either
//! applies its result to the project (`git apply --check` first, working
//! tree only, never the index or a commit), keeps it on its branch, or
//! discards it. The worktree is removed in every case and the conversation
//! moves back to the project, so later turns run in the main checkout.
//!
//! A worktree task's folder is never listed as a project or remembered as
//! the relaunch folder (see `Service::select_if`); its conversation is
//! listed under its project (`worktree_source`).
use crate::{
    compare::{self, Base, FileStat},
    engine::{Engine, Job},
    store::{keys, Store},
    workspace::Workspace,
    worktrees,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;

mod locks;
#[cfg(test)]
pub(crate) fn record_lock_references(store: &Store, id: &str) -> usize {
    locks::record_references(store, id)
}
const INDEXED: usize = 100;
const LISTED: usize = 30;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub id: String,
    /// The project the task belongs to (the main checkout).
    pub workspace: PathBuf,
    pub session_id: String,
    pub worktree: PathBuf,
    pub worktree_id: String,
    pub branch: String,
    pub base: Base,
    /// The first message, for lists.
    pub task: String,
    pub created_at: f64,
    pub finished_at: Option<f64>,
    /// starting | running | done | applied | branch | discarded
    pub state: String,
    /// The conversation's latest job and its status.
    pub job_id: String,
    pub status: String,
    pub changed_files: Vec<FileStat>,
    pub changed_files_truncated: bool,
    /// Files written to the project by "Apply to project".
    pub applied_files: Vec<String>,
    /// Files `git apply --check` refused on the last apply; nothing was
    /// written. Empty once an apply succeeds.
    pub conflicts: Vec<String>,
    pub conflict_detail: String,
    /// The branch holding the result, after "Keep as branch".
    pub kept_branch: Option<String>,
    /// Cleanup problems and a worktree deleted outside ShadowCode.
    pub notes: Vec<String>,
    /// The worktree was removed.
    pub removed: bool,
    /// Job id and finish time the stored diffstat belongs to (internal).
    stats_for: String,
}
impl Record {
    pub fn to_json(&self) -> Value {
        let mut value = json!(self);
        if let Some(map) = value.as_object_mut() {
            map.remove("stats_for");
        }
        value
    }
    /// Its worktree still exists and its conversation runs there.
    pub fn open(&self) -> bool {
        matches!(self.state.as_str(), "starting" | "running" | "done")
    }
}

/// Release record ownership before its outer task-repository guard. Core
/// worktree calls take a separate inner guard and cannot recurse into this one.
struct Operation {
    _record: OwnedMutexGuard<()>,
    _repository: Option<locks::Repository>,
}
fn same_identity(before: &Record, after: &Record) -> Result<()> {
    ensure!(
        before.id == after.id
            && before.workspace == after.workspace
            && before.worktree == after.worktree
            && before.worktree_id == after.worktree_id
            && before.session_id == after.session_id
            && before.branch == after.branch
            && before.base.commit == after.base.commit
            && before.base.head == after.base.head,
        "Worktree task identity changed while waiting; refresh before trying again"
    );
    Ok(())
}
fn pending_cleanup(record: &mut Record, error: &anyhow::Error) {
    let note = format!("Cleanup pending; task and recovery material kept: {error:#}")
        .chars()
        .take(2000)
        .collect::<String>();
    if !record.notes.contains(&note) {
        record.notes.push(note);
    }
}
fn managed_binding(engine: &Engine, record: &Record, common: &Path) -> Result<()> {
    let managed = worktrees::task_record(engine.paths(), &record.worktree_id)?;
    ensure!(
        managed.source == record.workspace
            && managed.path == record.worktree
            && managed.branch == record.branch
            && managed.common_directory == common,
        "Worktree task no longer matches its managed source/checkout; preserve for review"
    );
    Ok(())
}
async fn operation(
    engine: &Engine,
    id: &str,
    cancel: &CancellationToken,
) -> Result<(Operation, Record)> {
    let store = engine.store();
    let before = load(&store, id)?;
    // Completed acknowledgements and reads must survive a missing source.
    if !before.open() && before.removed {
        let guard = locks::record(&store, id, cancel).await?;
        let record = load(&store, id)?;
        same_identity(&before, &record)?;
        ensure!(
            !record.open() && record.removed,
            "Completed worktree task state changed; refresh before trying again"
        );
        return Ok((
            Operation {
                _record: guard,
                _repository: None,
            },
            record,
        ));
    }
    let repository = locks::repository(&before.workspace, cancel).await;
    let guard = locks::record(&store, id, cancel).await?;
    let mut record = load(&store, id)?;
    same_identity(&before, &record)?;
    if !record.open() && record.removed {
        return Ok((
            Operation {
                _record: guard,
                _repository: repository.ok(),
            },
            record,
        ));
    }
    let repository = match repository {
        Ok(repository) => repository,
        Err(error) => {
            // No Git/source authority is guessed from the remaining checkout.
            // Only this record's diagnostic is changed, under its record guard.
            ensure!(!cancel.is_cancelled(), "Worktree task operation cancelled");
            pending_cleanup(&mut record, &error);
            save(&store, &record)?;
            return Err(error.context(
                "Worktree task retained because source ownership could not be established",
            ));
        }
    };
    if !record.removed {
        if let Err(error) = managed_binding(engine, &record, &repository.common) {
            pending_cleanup(&mut record, &error);
            save(&store, &record)?;
            return Err(error);
        }
    }
    Ok((
        Operation {
            _record: guard,
            _repository: Some(repository),
        },
        record,
    ))
}

fn active(status: &str) -> bool {
    matches!(status, "queued" | "running" | "paused" | "cancelling")
}
fn valid_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "Unknown worktree task"
    );
    Ok(())
}
pub fn load(store: &Store, id: &str) -> Result<Record> {
    valid_id(id)?;
    let text = store
        .native_meta(&keys::worktree_task_record(id))?
        .context("Worktree task not found")?;
    let record: Record = serde_json::from_str(&text)?;
    ensure!(
        record.id == id,
        "Worktree task record identity changed; preserve for review"
    );
    Ok(record)
}
/// Store the record; a new one joins its project's index in the same
/// transaction.
fn save(store: &Store, record: &Record) -> Result<()> {
    let key = keys::worktree_task_record(&record.id);
    store.meta_transaction(|meta| {
        if meta.get(&key)?.is_none() {
            let index = keys::worktree_task_index(&record.workspace);
            let mut ids: Vec<String> = meta
                .get(&index)?
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            ids.retain(|id| id != &record.id);
            ids.insert(0, record.id.clone());
            ids.truncate(INDEXED);
            meta.set_json(&index, &ids)?;
        }
        meta.set_json(&key, record)
    })
}
fn index(store: &Store, workspace: &Path) -> Result<Vec<String>> {
    Ok(store
        .native_meta(&keys::worktree_task_index(workspace))?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default())
}
/// Remove a record that never started (and its index entry).
fn forget(store: &Store, record: &Record) -> Result<()> {
    store.meta_transaction(|meta| {
        let index = keys::worktree_task_index(&record.workspace);
        let mut ids: Vec<String> = meta
            .get(&index)?
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        ids.retain(|id| id != &record.id);
        meta.set_json(&index, &ids)?;
        meta.set(&keys::worktree_task_record(&record.id), "null")
    })
}

/// The open worktree task a conversation runs in, if any.
pub fn session_task(store: &Store, session_id: &str) -> Result<Option<String>> {
    store.session_meta(session_id, keys::WORKTREE_TASK)
}

/// A conversation whose worktree still exists is not deleted with it: the
/// worktree would be left behind without a way to reach it.
pub fn ensure_deletable(store: &Store, session_id: &str) -> Result<()> {
    if let Some(id) = session_task(store, session_id)? {
        if load(store, &id).is_ok_and(|record| record.open() && record.state != "starting") {
            anyhow::bail!(
                "This conversation still has its own worktree. Apply it to the project, keep it as a branch or discard it first."
            );
        }
    }
    Ok(())
}

/// Only one local model fits in GPU memory at a time: a worktree task on a
/// local model may not start while a task elsewhere uses a different one.
pub fn ensure_one_local_model(engine: &Engine, target: Option<&str>) -> Result<()> {
    let Some(target) = target.filter(|id| id.starts_with("local:gguf:")) else {
        return Ok(());
    };
    let store = engine.store();
    for job in store.job_summaries(100)? {
        if !active(job["status"].as_str().unwrap_or("")) {
            continue;
        }
        let Some(session) = job["session_id"].as_str() else {
            continue;
        };
        if let Some(other) = store.session_meta(session, keys::EXECUTION_TARGET)? {
            ensure!(
                !other.starts_with("local:gguf:") || other == target,
                "Another task is using a different local model, and only one local model fits in GPU memory at a time. Choose the same local model, a cloud or subscription model, or wait for the other task."
            );
        }
    }
    Ok(())
}

/// Copy the composer's attachments (`.shadow/attachments/…`, named in the
/// task or passed as images) into the worktree; they are usually ignored
/// files, so the snapshot does not carry them.
fn copy_attachments(source: &Path, worktree: &Path, task: &str, images: &[String]) -> Result<()> {
    const PREFIX: &str = ".shadow/attachments/";
    let mut wanted: Vec<String> = images.to_vec();
    let mut rest = task;
    while let Some(at) = rest.find(PREFIX) {
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == ',')
            .unwrap_or(tail.len());
        wanted.push(tail[..end].to_owned());
        rest = &tail[end..];
    }
    // The attachments folder must be a real directory inside the project; a
    // symlinked `.shadow/attachments` (or a symlink inside it) pointing at, say,
    // ~/.ssh must never be read, and must never be copied onto through the same
    // symlink in the worktree (which would truncate the target).
    let base = source.join(PREFIX);
    let (Ok(canon_base), Ok(canon_source)) = (base.canonicalize(), source.canonicalize()) else {
        return Ok(());
    };
    if !canon_base.starts_with(&canon_source) || !canon_base.is_dir() {
        return Ok(());
    }
    for relative in wanted {
        let path = Path::new(&relative);
        if !relative.starts_with(PREFIX)
            || path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            continue;
        }
        let from = source.join(path);
        // Resolve every component: a symlink anywhere in the path that leaves
        // the attachments folder is refused.
        let Ok(canon_from) = from.canonicalize() else {
            continue;
        };
        if !canon_from.starts_with(&canon_base) {
            continue;
        }
        match fs::symlink_metadata(&canon_from) {
            Ok(meta) if meta.is_file() => {}
            _ => continue,
        }
        let to = worktree.join(path);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
            // The destination folder must resolve inside the worktree, so the
            // copy cannot follow a symlink out of it.
            match (parent.canonicalize(), worktree.canonicalize()) {
                (Ok(canon_parent), Ok(canon_worktree))
                    if canon_parent.starts_with(&canon_worktree) => {}
                _ => continue,
            }
        }
        fs::copy(&canon_from, &to)
            .with_context(|| format!("Could not copy the attachment {relative}"))?;
    }
    Ok(())
}

/// Create the worktree and its conversation. The caller starts the first
/// turn there and then calls `started`, or `abandon` when it could not.
pub(crate) async fn prepare(
    engine: &Engine,
    source: &Path,
    task: &str,
    images: &[String],
    model: &str,
) -> Result<Record> {
    let cancel = CancellationToken::new();
    let source = Workspace::open(source)?.path;
    let repository = locks::repository(&source, &cancel).await?;
    compare::repository_root_for(
        &source,
        "Running in a new worktree needs",
        "running a task in a new worktree",
        &cancel,
    )
    .await?;
    let base = compare::snapshot_for(
        &engine.paths().data,
        &source,
        "running a task in a new worktree",
        "ShadowCode worktree task base",
        &cancel,
    )
    .await?;
    let checkout = worktrees::create(engine.paths(), &source, &base.commit, cancel.clone()).await?;
    let mut record = Record {
        id: crate::id(),
        workspace: source.clone(),
        worktree: checkout.path.clone(),
        worktree_id: checkout.id.clone(),
        branch: checkout.branch.clone(),
        base,
        task: task.chars().take(512).collect(),
        created_at: crate::now(),
        state: "starting".into(),
        status: "queued".into(),
        ..Default::default()
    };
    let record_guard = locks::record(&engine.store(), &record.id, &cancel).await?;
    let prepared: Result<()> = async {
        record.worktree = checkout.path.canonicalize()?;
        compare::set_trust(engine, std::slice::from_ref(&record.worktree), true)?;
        copy_attachments(&source, &record.worktree, task, images)?;
        let store = engine.store();
        let session = store.create_session(&record.worktree, model, "")?;
        record.session_id = session["id"]
            .as_str()
            .context("Missing session ID")?
            .to_owned();
        store.set_session_meta(&record.session_id, keys::WORKTREE_TASK, &record.id)?;
        store.set_session_meta(
            &record.session_id,
            keys::WORKTREE_SOURCE,
            &source.to_string_lossy(),
        )?;
        save(&store, &record)
    }
    .await;
    if let Err(error) = prepared {
        drop(record_guard);
        drop(repository);
        abandon(engine, record).await;
        return Err(error);
    }
    Ok(record)
}

/// The first turn started in the worktree.
pub(crate) async fn started(engine: &Engine, supplied: Record, job: &Job) -> Result<Record> {
    let store = engine.store();
    let _guard = locks::record(&store, &supplied.id, &CancellationToken::new()).await?;
    let mut record = load(&store, &supplied.id)?;
    same_identity(&supplied, &record)?;
    ensure!(
        record.state == "starting" && record.session_id == job.session_id,
        "Worktree task no longer awaits this initial job"
    );
    record.job_id = job.id.clone();
    record.status = job.status.clone();
    record.state = "running".into();
    let store = engine.store();
    let saved = record.clone();
    store.run(move |store| save(store, &saved)).await?;
    Ok(record)
}

/// The first turn could not start: remove its worktree and conversation when
/// cleanup succeeds; otherwise retain a visible task linked to recovery material.
pub(crate) async fn abandon(engine: &Engine, supplied: Record) {
    let cancel = CancellationToken::new();
    let store = engine.store();
    let repository = locks::repository(&supplied.workspace, &cancel).await;
    let Ok(_record) = locks::record(&store, &supplied.id, &cancel).await else {
        return;
    };
    let mut record = match store.native_meta(&keys::worktree_task_record(&supplied.id)) {
        Ok(Some(text)) if text == "null" => return, // already forgotten by another caller
        Ok(Some(_)) => match load(&store, &supplied.id) {
            Ok(record) if same_identity(&supplied, &record).is_ok() => record,
            _ => return, // foreign/malformed metadata is never replaced
        },
        Ok(None) => supplied,
        Err(_) => return,
    };
    let removed: Result<()> = async {
        let repository = repository
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))?;
        managed_binding(engine, &record, &repository.common)?;
        let _checkout = engine.reserve_workspace(&record.worktree)?;
        let _background = engine
            .background()
            .reserve_idle_workspace(&record.worktree)?;
        if !record.removed {
            worktrees::dispose(
                engine.paths(),
                &record.workspace,
                &record.worktree_id,
                cancel,
            )
            .await?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = removed {
        // Failed startup cleanup must not lose its only task-level recovery link.
        pending_cleanup(&mut record, &error);
        record.state = "done".into();
        record.status = "failed".into();
        record.finished_at.get_or_insert_with(crate::now);
        let _ = save(&store, &record);
        return;
    }
    let _ = compare::set_trust(engine, std::slice::from_ref(&record.worktree), false);
    let _ = forget(&store, &record);
    if !record.session_id.is_empty() {
        let _ = engine.delete_session(&record.session_id);
    }
}

/// Bring status and changed files up to date; `running` ⇄ `done` follows the
/// conversation's latest job (a follow-up turn runs it again).
async fn refresh(engine: &Engine, record: &mut Record, cancel: &CancellationToken) -> Result<()> {
    if !record.open() || record.state == "starting" {
        return Ok(());
    }
    let store = engine.store();
    let latest = store
        .session_jobs(&record.session_id, 1)?
        .pop()
        .and_then(|job| job["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| record.job_id.clone());
    let job = match engine.job(&latest)? {
        Some(job) => job,
        None => {
            record.status = "failed".into();
            return Ok(());
        }
    };
    record.job_id = job.id.clone();
    record.status = job.status.clone();
    if active(&job.status) {
        record.state = "running".into();
        record.finished_at = None;
    } else if record.state == "running" {
        record.state = "done".into();
        record.finished_at = job.finished_at.or_else(|| Some(crate::now()));
    }
    if !record.worktree.is_dir() {
        let note = format!(
            "Its worktree {} was deleted outside ShadowCode",
            record.worktree.display()
        );
        if !record.notes.contains(&note) {
            record.notes.push(note);
        }
        return Ok(());
    }
    let key = format!("{}:{:?}", job.id, job.finished_at);
    if active(&job.status) || record.stats_for != key {
        if let Ok((files, truncated)) =
            compare::diffstat(&record.worktree, &record.base.commit, cancel).await
        {
            record.changed_files = files;
            record.changed_files_truncated = truncated;
            if !active(&job.status) {
                record.stats_for = key;
            }
        }
    }
    Ok(())
}

pub async fn get(engine: &Engine, id: &str) -> Result<Record> {
    let store = engine.store();
    let _guard = locks::record(&store, id, &CancellationToken::new()).await?;
    let mut record = load(&store, id)?;
    refresh(engine, &mut record, &CancellationToken::new()).await?;
    save(&store, &record)?;
    Ok(record)
}

/// The project's worktree tasks, newest first; open ones are refreshed.
pub async fn list(engine: &Engine, workspace: &Path) -> Result<Vec<Record>> {
    let workspace = Workspace::open(workspace)?.path;
    let store = engine.store();
    let mut records = Vec::new();
    for id in index(&store, &workspace)?.iter().take(LISTED) {
        if valid_id(id).is_err() {
            continue;
        }
        let _guard = locks::record(&store, id, &CancellationToken::new()).await?;
        let Ok(mut record) = load(&store, id) else {
            continue;
        };
        if record.open() {
            refresh(engine, &mut record, &CancellationToken::new()).await?;
            save(&store, &record)?;
        }
        records.push(record);
    }
    Ok(records)
}

/// A worktree task that can be closed now: open, with no turn running.
async fn closable(
    engine: &Engine,
    mut record: Record,
    cancel: &CancellationToken,
) -> Result<Record> {
    ensure!(
        record.open() && record.state != "starting",
        "This worktree task was already {}",
        closed_label(&record.state)
    );
    refresh(engine, &mut record, cancel).await?;
    Ok(record)
}
fn closed_label(state: &str) -> &'static str {
    match state {
        "applied" => "applied to the project",
        "branch" => "kept as a branch",
        "discarded" => "discarded",
        _ => "closed",
    }
}

/// "Apply to project": commit the result on the worktree's own branch, then
/// apply it to the project's working tree. When `git apply --check` refuses,
/// nothing is written and the record lists the conflicting files (state
/// stays `done`); otherwise the worktree is removed and the conversation
/// returns to the project.
pub async fn apply(engine: &Engine, id: &str) -> Result<Record> {
    let cancel = CancellationToken::new();
    let (_guard, record) = operation(engine, id, &cancel).await?;
    let store = engine.store();
    let mut record = closable(engine, record, &cancel).await?;
    ensure!(
        !active(&record.status),
        "The task is still working. Wait for it to finish or stop it first."
    );
    ensure!(
        record.worktree.is_dir(),
        "Its worktree was deleted outside ShadowCode, so its result cannot be applied. Discard it instead."
    );
    // No task may run in the project, or in the worktree, while this runs.
    let _project = engine
        .reserve_workspace(&record.workspace)
        .map_err(|error| {
            anyhow::anyhow!(
                "{error:#}. Stop or wait for the task running in the project, then apply again."
            )
        })?;
    let _checkout = engine.reserve_workspace(&record.worktree)?;
    let _background = engine
        .background()
        .reserve_idle_workspace(&record.worktree)?;
    worktrees::validate_task_checkout(
        engine.paths(),
        &record.workspace,
        &record.worktree_id,
        &record.worktree,
        &record.base.commit,
        &record.branch,
        &cancel,
    )
    .await?;
    let _source_background = engine
        .background()
        .reserve_idle_workspace(&record.workspace)?;
    let (head, files) = compare::commit_checkout(
        &record.worktree,
        &record.base.commit,
        "ShadowCode worktree task result",
        &cancel,
    )
    .await?;
    if !files.is_empty() {
        if let Some(refused) = compare::apply_checkout(
            &engine.paths().data,
            &record.worktree,
            &record.workspace,
            &record.base.commit,
            &head,
            &cancel,
        )
        .await?
        {
            record.conflicts = refused.conflicts;
            record.conflict_detail = refused.detail;
            save(&store, &record)?;
            return Ok(record);
        }
    }
    record.conflicts.clear();
    record.conflict_detail.clear();
    record.applied_files = files;
    close(engine, &mut record, "applied").await;
    save(&store, &record)?;
    Ok(record)
}

/// "Keep as branch": commit the result on the worktree's branch
/// (`shadowcode/<id>`), remove the worktree and keep the branch.
pub async fn keep_branch(engine: &Engine, id: &str) -> Result<Record> {
    let cancel = CancellationToken::new();
    let (_guard, record) = operation(engine, id, &cancel).await?;
    let store = engine.store();
    let mut record = closable(engine, record, &cancel).await?;
    ensure!(
        !active(&record.status),
        "The task is still working. Wait for it to finish or stop it first."
    );
    ensure!(
        record.worktree.is_dir(),
        "Its worktree was deleted outside ShadowCode, so there is nothing to keep. Discard it instead."
    );
    let _checkout = engine.reserve_workspace(&record.worktree)?;
    let _background = engine
        .background()
        .reserve_idle_workspace(&record.worktree)?;
    worktrees::validate_task_checkout(
        engine.paths(),
        &record.workspace,
        &record.worktree_id,
        &record.worktree,
        &record.base.commit,
        &record.branch,
        &cancel,
    )
    .await?;
    let first_line = record.task.lines().next().unwrap_or("").trim();
    let message = if first_line.is_empty() {
        "ShadowCode worktree task result".to_owned()
    } else {
        format!(
            "ShadowCode: {}",
            first_line.chars().take(72).collect::<String>()
        )
    };
    compare::commit_checkout(&record.worktree, &record.base.commit, &message, &cancel).await?;
    record.kept_branch = Some(record.branch.clone());
    close(engine, &mut record, "branch").await;
    save(&store, &record)?;
    Ok(record)
}

/// Record-only phase: stopping an already-recorded job does not need a live
/// source repository. Callers release this record guard before requesting outer
/// repository ownership; no record -> repository wait is permitted.
async fn stop_for_discard(
    engine: &Engine,
    record: &mut Record,
    cancel: &CancellationToken,
) -> Result<crate::engine::WorkspaceReservation> {
    let reservation = engine.stop_and_reserve_workspace(&record.worktree).await?;
    refresh(engine, record, cancel).await?;
    Ok(reservation)
}

/// "Discard": stop a running turn, remove the worktree and its branch.
pub async fn discard(engine: &Engine, id: &str) -> Result<Record> {
    let cancel = CancellationToken::new();
    {
        let store = engine.store();
        let _record = locks::record(&store, id, &cancel).await?;
        let mut record = load(&store, id)?;
        if record.open() {
            stop_for_discard(engine, &mut record, &cancel).await?;
            save(&store, &record)?;
        }
    } // Release before acquiring task repository ownership, including on errors.
    let (_guard, mut record) = operation(engine, id, &cancel).await?;
    let store = engine.store();
    if !record.open() {
        if !record.removed && !record.worktree_id.is_empty() {
            // Retry the cleanup a previous attempt left behind.
            let _checkout = stop_for_discard(engine, &mut record, &cancel).await?;
            let _background = engine
                .background()
                .reserve_idle_workspace(&record.worktree)?;
            let state = record.state.clone();
            close(engine, &mut record, &state).await;
            save(&store, &record)?;
            return load(&store, id);
        }
        return Ok(record);
    }
    // Close admission before stopping every active/queued turn, and retain
    // ownership through cleanup and the conversation move.
    let _checkout = stop_for_discard(engine, &mut record, &cancel).await?;
    let _background = engine
        .background()
        .reserve_idle_workspace(&record.worktree)?;
    close(engine, &mut record, "discarded").await;
    save(&store, &record)?;
    load(&store, id)
}

/// Remove the worktree (keeping its branch for `branch`), stop trusting its
/// folder and move the conversation back to the project.
async fn close(engine: &Engine, record: &mut Record, state: &str) {
    record.state = state.into();
    record.finished_at.get_or_insert_with(crate::now);
    if !record.removed && !record.worktree_id.is_empty() {
        let removed = if state == "branch" {
            worktrees::release(
                engine.paths(),
                &record.workspace,
                &record.worktree_id,
                CancellationToken::new(),
            )
            .await
        } else {
            worktrees::dispose(
                engine.paths(),
                &record.workspace,
                &record.worktree_id,
                CancellationToken::new(),
            )
            .await
        };
        match removed {
            Ok(note) => {
                record.removed = true;
                record.notes.extend(note);
            }
            Err(error) => record.notes.push(format!(
                "The worktree {} was kept: {error:#}",
                record.worktree.display()
            )),
        }
        if let Err(error) =
            compare::set_trust(engine, std::slice::from_ref(&record.worktree), false)
        {
            record
                .notes
                .push(format!("Could not update trusted projects: {error:#}"));
        }
    }
    let store = engine.store();
    let moved = store
        .move_session(&record.session_id, &record.workspace)
        .and_then(|()| {
            store.delete_session_meta(&record.session_id, keys::WORKTREE_TASK)?;
            store.delete_session_meta(&record.session_id, keys::WORKTREE_SOURCE)
        });
    if let Err(error) = moved {
        record.notes.push(format!(
            "The conversation could not move back to the project: {error:#}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn attachments_named_in_the_task_are_copied() {
        let root = tempfile::tempdir().unwrap();
        let (source, worktree) = (root.path().join("s"), root.path().join("w"));
        fs::create_dir_all(source.join(".shadow/attachments")).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        fs::write(source.join(".shadow/attachments/a-notes.txt"), "notes").unwrap();
        fs::write(source.join(".shadow/attachments/b-shot.png"), "png").unwrap();
        fs::write(source.join("secret.txt"), "no").unwrap();
        copy_attachments(
            &source,
            &worktree,
            "Fix it\n\nAttached paths: .shadow/attachments/a-notes.txt, .shadow/attachments/../../secret.txt",
            &[".shadow/attachments/b-shot.png".into()],
        )
        .unwrap();
        assert!(worktree.join(".shadow/attachments/a-notes.txt").is_file());
        assert!(worktree.join(".shadow/attachments/b-shot.png").is_file());
        assert!(!worktree.join("secret.txt").exists());
    }

    #[test]
    fn attachments_never_follow_a_symlink_out_of_the_project() {
        let root = tempfile::tempdir().unwrap();
        let (source, worktree) = (root.path().join("s"), root.path().join("w"));
        let outside = root.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("id_rsa");
        fs::write(&secret, "PRIVATE KEY CONTENT").unwrap();

        // (a) the whole attachments folder is a symlink to a folder outside.
        fs::create_dir_all(source.join(".shadow")).unwrap();
        std::os::unix::fs::symlink(&outside, source.join(".shadow/attachments")).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        copy_attachments(&source, &worktree, "read .shadow/attachments/id_rsa", &[]).unwrap();
        assert!(!worktree.join(".shadow/attachments/id_rsa").exists());
        assert_eq!(fs::read_to_string(&secret).unwrap(), "PRIVATE KEY CONTENT");

        // (b) a symlink inside a real attachments folder points outside.
        fs::remove_file(source.join(".shadow/attachments")).unwrap();
        fs::create_dir_all(source.join(".shadow/attachments")).unwrap();
        std::os::unix::fs::symlink(&secret, source.join(".shadow/attachments/key")).unwrap();
        let worktree2 = root.path().join("w2");
        fs::create_dir_all(&worktree2).unwrap();
        copy_attachments(&source, &worktree2, "use .shadow/attachments/key", &[]).unwrap();
        assert!(!worktree2.join(".shadow/attachments/key").exists());
        assert_eq!(fs::read_to_string(&secret).unwrap(), "PRIVATE KEY CONTENT");
    }

    #[test]
    fn records_open_until_closed() {
        let mut record = Record {
            state: "done".into(),
            ..Default::default()
        };
        assert!(record.open());
        for state in ["applied", "branch", "discarded"] {
            record.state = state.into();
            assert!(!record.open());
        }
        assert!(record.to_json().get("stats_for").is_none());
        assert!(valid_id("../x").is_err());
    }
    fn command(source: &Path, args: &[&str]) -> String {
        let result = std::process::Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Task Lock Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(source)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    }
    struct TaskFixture {
        engine: Engine,
        root: tempfile::TempDir,
        source: PathBuf,
        record: Record,
    }
    async fn fixture() -> TaskFixture {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        command(&source, &["init", "-q"]);
        fs::write(source.join("tracked.txt"), b"source bytes\n").unwrap();
        command(&source, &["add", "tracked.txt"]);
        command(&source, &["commit", "-qm", "Base"]);
        let paths = crate::paths::AppPaths::isolated(&root.path().join("profile")).unwrap();
        crate::config::Config::patch(&paths, json!({
            "cli_agents":{"enabled":false},
            "model":{"provider":"local","name":"fixture-only","default":"fixture-only","endpoint":"http://127.0.0.1:9/v1","context_limit":16384}
        })).unwrap();
        let engine = Engine::open(paths).unwrap();
        let mut record = prepare(
            &engine,
            &source,
            "Fixture without a model turn",
            &[],
            "fixture-only",
        )
        .await
        .unwrap();
        record.state = "done".into();
        record.status = "completed".into();
        save(&engine.store(), &record).unwrap();
        TaskFixture {
            engine,
            root,
            source,
            record,
        }
    }
    async fn until(mut ready: impl FnMut() -> bool, message: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !ready() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect(message);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_task_record_waiter_does_not_change_or_poison_the_record() {
        let f = fixture().await;
        let store = f.engine.store();
        let before = store
            .native_meta(&keys::worktree_task_record(&f.record.id))
            .unwrap();
        let held = locks::record(&store, &f.record.id, &CancellationToken::new())
            .await
            .unwrap();
        let token = CancellationToken::new();
        let waiting_token = token.clone();
        let waiting_store = store.clone();
        let waiting_id = f.record.id.clone();
        let waiting = tokio::spawn(async move {
            locks::record(&waiting_store, &waiting_id, &waiting_token)
                .await
                .map(|_| ())
        });
        until(
            || locks::record_references(&store, &f.record.id) >= 2,
            "Record waiter never entered queue",
        )
        .await;
        token.cancel();
        let error = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error:#}");
        assert_eq!(
            store
                .native_meta(&keys::worktree_task_record(&f.record.id))
                .unwrap(),
            before
        );
        drop(held);
        let record = tokio::time::timeout(Duration::from_secs(1), get(&f.engine, &f.record.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.id, f.record.id);
        assert!(discard(&f.engine, &f.record.id).await.unwrap().removed);
        f.engine.shutdown().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_common_directory_alias_waiters_cancel_without_poisoning_unrelated_preparation() {
        let f = fixture().await;
        let alias = f.root.path().join("alias");
        std::os::unix::fs::symlink(&f.source, &alias).unwrap();
        let linked = f.root.path().join("linked");
        command(
            &f.source,
            &[
                "worktree",
                "add",
                "--detach",
                linked.to_str().unwrap(),
                "HEAD",
            ],
        );
        let held = locks::repository(&f.source, &CancellationToken::new())
            .await
            .unwrap();
        for source in [alias, linked] {
            let token = CancellationToken::new();
            let waiting_token = token.clone();
            let waiting = tokio::spawn(async move {
                locks::repository(&source, &waiting_token).await.map(|_| ())
            });
            until(
                || locks::repository_references(&held.common) >= 2,
                "Common-directory alias never reached queue",
            )
            .await;
            token.cancel();
            let error = tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(error.to_string().contains("cancelled"), "{error:#}");
        }
        let second = f.root.path().join("second");
        fs::create_dir(&second).unwrap();
        command(&second, &["init", "-q"]);
        fs::write(second.join("other.txt"), b"independent bytes\n").unwrap();
        command(&second, &["add", "other.txt"]);
        command(&second, &["commit", "-qm", "Independent"]);
        let prepared = tokio::time::timeout(
            Duration::from_secs(2),
            prepare(&f.engine, &second, "Independent", &[], "fixture-only"),
        )
        .await
        .expect("Other repository waited for held task repository ownership")
        .unwrap();
        abandon(&f.engine, prepared).await;
        assert_eq!(
            fs::read(second.join("other.txt")).unwrap(),
            b"independent bytes\n"
        );
        drop(held);
        assert!(discard(&f.engine, &f.record.id).await.unwrap().removed);
        f.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn missing_source_keeps_task_and_admin_recovery_reachable_until_the_original_returns() {
        let f = fixture().await;
        let preserved = f.root.path().join("preserved-source");
        let before_head = command(&f.source, &["rev-parse", "HEAD"]);
        let source_index = fs::read(f.source.join(".git/index")).unwrap();
        let admin_index = fs::read(
            f.source
                .join(".git/worktrees")
                .join(&f.record.worktree_id)
                .join("index"),
        )
        .unwrap();
        let pointer = fs::read(f.record.worktree.join(".git")).unwrap();
        fs::rename(&f.source, &preserved).unwrap();
        let visible = get(&f.engine, &f.record.id).await.unwrap();
        assert_eq!(visible.id, f.record.id);
        let error = discard(&f.engine, &f.record.id).await.unwrap_err();
        assert!(error.to_string().contains("source ownership"), "{error:#}");
        let pending = get(&f.engine, &f.record.id).await.unwrap();
        assert!(pending.open() && !pending.removed);
        assert!(pending
            .notes
            .iter()
            .any(|note| note.contains("Cleanup pending")));
        // A startup failure cleanup follows the same rule: do not forget the
        // task/conversation just because its source authority is unavailable.
        abandon(&f.engine, pending).await;
        let kept = load(&f.engine.store(), &f.record.id).unwrap();
        assert!(kept.open() && !kept.removed);
        assert_eq!(
            session_task(&f.engine.store(), &f.record.session_id)
                .unwrap()
                .as_deref(),
            Some(f.record.id.as_str())
        );
        assert!(f
            .engine
            .store()
            .session(&f.record.session_id)
            .unwrap()
            .is_some());
        assert_eq!(
            fs::read(preserved.join(".git/index")).unwrap(),
            source_index
        );
        assert_eq!(
            fs::read(
                preserved
                    .join(".git/worktrees")
                    .join(&f.record.worktree_id)
                    .join("index")
            )
            .unwrap(),
            admin_index
        );
        assert_eq!(fs::read(f.record.worktree.join(".git")).unwrap(), pointer);
        assert_eq!(
            fs::read(f.record.worktree.join("tracked.txt")).unwrap(),
            b"source bytes\n"
        );
        let managed = worktrees::task_record(f.engine.paths(), &f.record.worktree_id).unwrap();
        assert_eq!(
            managed.source, f.source,
            "Never substitute the retained directory as new source authority"
        );
        fs::rename(&preserved, &f.source).unwrap();
        let closed = discard(&f.engine, &f.record.id).await.unwrap();
        assert!(closed.removed && !closed.open());
        assert_eq!(command(&f.source, &["rev-parse", "HEAD"]), before_head);
        assert_eq!(fs::read(f.source.join(".git/index")).unwrap(), source_index);
        assert!(!f.record.worktree.exists());
        // A completed acknowledgement needs no live source or new Git action.
        fs::rename(&f.source, &preserved).unwrap();
        let acknowledged = discard(&f.engine, &f.record.id).await.unwrap();
        assert_eq!(acknowledged.to_json(), closed.to_json());
        assert_eq!(
            get(&f.engine, &f.record.id).await.unwrap().to_json(),
            closed.to_json()
        );
        fs::rename(&preserved, &f.source).unwrap();
        f.engine.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_identity_is_reloaded_after_repository_wait_without_replacing_foreign_metadata() {
        let f = fixture().await;
        let held = locks::repository(&f.source, &CancellationToken::new())
            .await
            .unwrap();
        let waiting_engine = f.engine.clone();
        let waiting_id = f.record.id.clone();
        let waiting = tokio::spawn(async move { discard(&waiting_engine, &waiting_id).await });
        until(
            || locks::repository_references(&held.common) >= 2,
            "Discard never reached repository queue",
        )
        .await;
        let mut changed = f.record.clone();
        changed.worktree_id = crate::id();
        save(&f.engine.store(), &changed).unwrap();
        let changed_bytes = f
            .engine
            .store()
            .native_meta(&keys::worktree_task_record(&changed.id))
            .unwrap();
        drop(held);
        let error = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("identity changed"), "{error:#}");
        assert_eq!(
            f.engine
                .store()
                .native_meta(&keys::worktree_task_record(&changed.id))
                .unwrap(),
            changed_bytes
        );
        assert_eq!(
            fs::read(f.record.worktree.join("tracked.txt")).unwrap(),
            b"source bytes\n"
        );
        save(&f.engine.store(), &f.record).unwrap();
        assert!(discard(&f.engine, &f.record.id).await.unwrap().removed);
        f.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn mismatched_stored_task_id_cannot_redirect_a_guarded_save() {
        let f = fixture().await;
        let store = f.engine.store();
        let key = keys::worktree_task_record(&f.record.id);
        let before = store.native_meta(&key).unwrap().unwrap();
        let mut foreign = f.record.clone();
        foreign.id = crate::id();
        let altered = serde_json::to_string(&foreign).unwrap();
        store.set_native_meta(&key, &altered).unwrap();
        assert!(get(&f.engine, &f.record.id)
            .await
            .unwrap_err()
            .to_string()
            .contains("identity changed"));
        assert!(discard(&f.engine, &f.record.id)
            .await
            .unwrap_err()
            .to_string()
            .contains("identity changed"));
        assert_eq!(
            store.native_meta(&key).unwrap().as_deref(),
            Some(altered.as_str())
        );
        assert!(store
            .native_meta(&keys::worktree_task_record(&foreign.id))
            .unwrap()
            .is_none());
        assert_eq!(
            fs::read(f.record.worktree.join("tracked.txt")).unwrap(),
            b"source bytes\n"
        );
        store.set_native_meta(&key, &before).unwrap();
        assert!(discard(&f.engine, &f.record.id).await.unwrap().removed);
        f.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn foreign_managed_source_refuses_before_committing_the_task_result() {
        let f = fixture().await;
        let foreign = f.root.path().join("foreign-source");
        fs::create_dir(&foreign).unwrap();
        command(&foreign, &["init", "-q"]);
        fs::write(foreign.join("foreign.txt"), b"foreign bytes\n").unwrap();
        command(&foreign, &["add", "foreign.txt"]);
        command(&foreign, &["commit", "-qm", "Foreign"]);
        let foreign_head = command(&foreign, &["rev-parse", "HEAD"]);
        let lane_head = command(&f.record.worktree, &["rev-parse", "HEAD"]);
        let lane_index = fs::read(
            f.source
                .join(".git/worktrees")
                .join(&f.record.worktree_id)
                .join("index"),
        )
        .unwrap();
        fs::write(
            f.record.worktree.join("tracked.txt"),
            b"uncommitted task result\n",
        )
        .unwrap();
        let record_path = f
            .engine
            .paths()
            .data
            .join("managed-worktrees/records")
            .join(format!("{}.json", f.record.worktree_id));
        let original = fs::read(&record_path).unwrap();
        let mut managed: worktrees::Record = serde_json::from_slice(&original).unwrap();
        managed.source = foreign.clone();
        managed.common_directory = foreign.join(".git");
        let changed = serde_json::to_vec_pretty(&managed).unwrap();
        fs::write(&record_path, &changed).unwrap();
        let error = keep_branch(&f.engine, &f.record.id).await.unwrap_err();
        assert!(
            error.to_string().contains("managed source/checkout"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&record_path).unwrap(),
            changed,
            "Never overwrite foreign recovery metadata"
        );
        assert_eq!(
            command(&f.record.worktree, &["rev-parse", "HEAD"]),
            lane_head
        );
        assert_eq!(
            fs::read(
                f.source
                    .join(".git/worktrees")
                    .join(&f.record.worktree_id)
                    .join("index")
            )
            .unwrap(),
            lane_index
        );
        assert_eq!(
            fs::read(f.record.worktree.join("tracked.txt")).unwrap(),
            b"uncommitted task result\n"
        );
        assert_eq!(command(&foreign, &["rev-parse", "HEAD"]), foreign_head);
        assert_eq!(
            fs::read(foreign.join("foreign.txt")).unwrap(),
            b"foreign bytes\n"
        );
        assert!(load(&f.engine.store(), &f.record.id).unwrap().open());
        fs::write(&record_path, original).unwrap();
        assert!(discard(&f.engine, &f.record.id).await.unwrap().removed);
        f.engine.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_source_discard_still_cancels_its_pending_command_without_executing_it() {
        use crate::engine::{CommandRequest, StartRequest};
        let f = fixture().await;
        crate::config::Config::patch(
            f.engine.paths(),
            json!({"permissions":{"mode":"ask","approve_shell":true}}),
        )
        .unwrap();
        let mut starting = f.record.clone();
        starting.state = "starting".into();
        starting.status = "queued".into();
        save(&f.engine.store(), &starting).unwrap();
        let job = f
            .engine
            .start_command(
                StartRequest {
                    workspace: f.record.worktree.clone(),
                    task: "Never execute the denied fixture command".into(),
                    session_id: Some(f.record.session_id.clone()),
                    model: None,
                    mode: "command".into(),
                    queue: false,
                    images: Vec::new(),
                    web: false,
                },
                CommandRequest {
                    command: "printf forbidden > must-not-run.txt".into(),
                    timeout_sec: 30,
                },
                None,
            )
            .await
            .unwrap();
        started(&f.engine, starting, &job).await.unwrap();
        until(
            || {
                !f.engine
                    .approvals()
                    .list(Some(&f.record.session_id))
                    .is_empty()
            },
            "Command did not reach its approval barrier",
        )
        .await;
        let preserved = f.root.path().join("preserved-active-source");
        fs::rename(&f.source, &preserved).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(3), discard(&f.engine, &f.record.id))
            .await
            .expect("Missing source prevented stopping the job")
            .unwrap_err();
        assert!(error.to_string().contains("source ownership"), "{error:#}");
        let ended = f.engine.job(&job.id).unwrap().unwrap();
        assert_eq!(ended.status, "cancelled");
        assert!(f
            .engine
            .approvals()
            .list(Some(&f.record.session_id))
            .is_empty());
        assert!(!f.record.worktree.join("must-not-run.txt").exists());
        let pending = get(&f.engine, &f.record.id).await.unwrap();
        assert!(pending.open() && !pending.removed);
        assert!(pending
            .notes
            .iter()
            .any(|note| note.contains("Cleanup pending")));
        assert_eq!(
            session_task(&f.engine.store(), &f.record.session_id)
                .unwrap()
                .as_deref(),
            Some(f.record.id.as_str())
        );
        fs::rename(&preserved, &f.source).unwrap();
        assert!(discard(&f.engine, &f.record.id).await.unwrap().removed);
        assert!(!f.source.join("must-not-run.txt").exists());
        f.engine.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn discard_stops_active_and_queued_turns_before_removing_checkout() {
        use crate::engine::{CommandRequest, StartRequest};
        let f = fixture().await;
        crate::config::Config::patch(
            f.engine.paths(),
            json!({"permissions":{"mode":"ask","approve_shell":true}}),
        )
        .unwrap();
        let mut jobs = Vec::new();
        for queued in [false, true] {
            let job = f
                .engine
                .start_command(
                    StartRequest {
                        workspace: f.record.worktree.clone(),
                        task: "Never execute".into(),
                        session_id: Some(f.record.session_id.clone()),
                        model: None,
                        mode: "command".into(),
                        queue: queued,
                        images: Vec::new(),
                        web: false,
                    },
                    CommandRequest {
                        command: "printf forbidden > must-not-run.txt".into(),
                        timeout_sec: 30,
                    },
                    None,
                )
                .await
                .unwrap();
            jobs.push(job);
            if !queued {
                until(
                    || {
                        !f.engine
                            .approvals()
                            .list(Some(&f.record.session_id))
                            .is_empty()
                    },
                    "Command never reached approval",
                )
                .await;
            }
        }
        assert_eq!(f.engine.job(&jobs[1].id).unwrap().unwrap().status, "queued");
        let result = discard(&f.engine, &f.record.id).await;
        let statuses: Vec<_> = jobs
            .iter()
            .map(|job| f.engine.job(&job.id).unwrap().unwrap().status)
            .collect();
        let did_not_run = !f.source.join("must-not-run.txt").exists()
            && !f.record.worktree.join("must-not-run.txt").exists();
        f.engine.shutdown().await.unwrap();
        assert!(result.unwrap().removed);
        assert_eq!(
            statuses,
            vec!["cancelled", "cancelled"],
            "Every active/queued turn must finish before checkout removal"
        );
        assert!(did_not_run);
    }

    #[tokio::test]
    async fn keep_branch_refuses_live_branch_change_before_staging() {
        let f = fixture().await;
        command(&f.record.worktree, &["switch", "-c", "user-owned"]);
        let before = command(&f.record.worktree, &["rev-parse", "HEAD"]);
        let admin = PathBuf::from(command(
            &f.record.worktree,
            &["rev-parse", "--absolute-git-dir"],
        ));
        let index = fs::read(admin.join("index")).unwrap();
        fs::write(
            f.record.worktree.join("tracked.txt"),
            "pending task edits\n",
        )
        .unwrap();
        let result = keep_branch(&f.engine, &f.record.id).await;
        let after = command(&f.source, &["rev-parse", "refs/heads/user-owned"]);
        let after_index = fs::read(admin.join("index")).unwrap();
        f.engine.shutdown().await.unwrap();
        assert!(result.is_err(), "Task committed on a user branch");
        assert_eq!(after, before);
        assert_eq!(after_index, index);
        assert!(f.record.worktree.exists());
    }
    #[tokio::test]
    async fn failed_initial_start_retains_failed_abandon_until_cleanup_retry() {
        use crate::engine::{CommandRequest, StartRequest};
        let f = fixture().await;
        let prepared = prepare(
            &f.engine,
            &f.source,
            "Startup failure fixture",
            &[],
            "fixture-only",
        )
        .await
        .unwrap();
        assert_eq!(prepared.state, "starting");
        let held = f.engine.reserve_workspace(&prepared.worktree).unwrap();
        let failed_start = f
            .engine
            .start_command(
                StartRequest {
                    workspace: prepared.worktree.clone(),
                    task: "Never run".into(),
                    session_id: Some(prepared.session_id.clone()),
                    model: None,
                    mode: "command".into(),
                    queue: false,
                    images: Vec::new(),
                    web: false,
                },
                CommandRequest {
                    command: "printf forbidden > must-not-run.txt".into(),
                    timeout_sec: 30,
                },
                None,
            )
            .await;
        assert!(failed_start
            .unwrap_err()
            .to_string()
            .contains("manual operation"));
        abandon(&f.engine, prepared.clone()).await;
        let retained = get(&f.engine, &prepared.id).await.unwrap();
        assert!(retained.open() && !retained.removed && retained.worktree.exists());
        assert_eq!(retained.status, "failed");
        assert!(retained
            .notes
            .iter()
            .any(|note| note.contains("manual operation")));
        assert_eq!(
            session_task(&f.engine.store(), &prepared.session_id)
                .unwrap()
                .as_deref(),
            Some(prepared.id.as_str())
        );
        drop(held);
        let retried = discard(&f.engine, &prepared.id).await.unwrap();
        assert!(retried.removed && !prepared.worktree.exists());
        assert!(!f.source.join("must-not-run.txt").exists());
        assert!(discard(&f.engine, &f.record.id).await.unwrap().removed);
        f.engine.shutdown().await.unwrap();
    }
}

//! Compare: one task sent to 2–3 models (managed local jobs are sequential). Each model ("lane") works
//! in its own managed Git worktree created from the same starting state (HEAD
//! plus the user's uncommitted, non-ignored work) and runs as a normal job.
//! Keeping a lane applies its changes to the source working tree with
//! `git apply` (never the index, never a commit) and removes every lane.
//!
//! The source checkout is never reset, stashed, checked out or committed in.
//! The starting state is captured with a temporary index file, so neither the
//! source index nor its working tree is written while the compare starts.
use crate::{
    config::{self, Config},
    engine::{Engine, Job, JobOwner, StartRequest},
    model_registry,
    process::{self, ProcessResult, ProcessSpec},
    store::{keys, MetaTransaction, Store},
    workspace::Workspace,
    worktrees,
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Weak},
    time::Duration,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Lock order: project metadata -> repository advisory lock -> engine
/// reservations -> worktree disposal. The registry lock is never held over
/// an await. Weak entries keep closed projects from accumulating locks.
static PROJECT_LOCKS: LazyLock<std::sync::Mutex<BTreeMap<PathBuf, Weak<Mutex<()>>>>> =
    LazyLock::new(|| std::sync::Mutex::new(BTreeMap::new()));

pub(crate) fn project_lock(workspace: &Path) -> Result<Arc<Mutex<()>>> {
    let mut locks = PROJECT_LOCKS
        .lock()
        .map_err(|_| anyhow::anyhow!("Compare lock registry poisoned"))?;
    locks.retain(|_, value| value.strong_count() > 0);
    let lock = locks
        .get(workspace)
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| Arc::new(Mutex::new(())));
    locks.insert(workspace.to_owned(), Arc::downgrade(&lock));
    Ok(lock)
}

/// Cross-profile/process mutation ownership. Never unlink this lock file:
/// removing a locked inode would allow another writer through a new inode.
async fn mutation_lock(workspace: &Path, cancel: &CancellationToken) -> Result<fs::File> {
    use fs2::FileExt;
    let directory = git(
        workspace,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cancel,
    )
    .await?;
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(Path::new(&directory).join("shadowcode-compare.lock"))?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Compare lock must be a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() } && metadata.nlink() == 1,
            "Compare lock must be owned by your account without extra hard links"
        );
    }
    file.try_lock_exclusive()
        .context("Another ShadowCode operation owns this repository; retry after it finishes")?;
    Ok(file)
}

/// Editor draft writes share Compare's cross-process admission lock when the
/// selected workspace belongs to a Git repository. Draft recovery also works
/// for standalone folders, where Compare cannot snapshot and no repository
/// lock is needed.
pub(crate) async fn workspace_mutation_lock(
    workspace: &Path,
    cancel: &CancellationToken,
) -> Result<Option<fs::File>> {
    let probe = git_with(workspace, &["rev-parse", "--show-toplevel"], &[], cancel).await?;
    if !probe.ok
        && !probe.truncated
        && probe
            .stderr
            .to_ascii_lowercase()
            .contains("not a git repository")
    {
        return Ok(None);
    }
    ensure!(
        probe.ok,
        "Could not establish editor draft repository ownership: {}{}",
        probe.stderr.trim(),
        probe.stdout.trim()
    );
    mutation_lock(workspace, cancel).await.map(Some)
}
const LISTED: usize = 20;
const INDEXED: usize = 100;
const MAX_FILES: usize = 1000;
const COUNT_LIMIT: u64 = 8_000_000;
const IDENTITY: [&str; 6] = [
    "-c",
    "user.name=ShadowCode",
    "-c",
    "user.email=shadowcode@localhost",
    "-c",
    "commit.gpgsign=false",
];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Base {
    /// The commit every lane starts from: HEAD, or a "ShadowCode compare
    /// base" commit on top of HEAD holding the uncommitted work.
    pub commit: String,
    pub head: String,
    pub included_uncommitted: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FileStat {
    pub path: String,
    /// added | modified | deleted
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckCommand {
    pub command: String,
    pub state: String,
    pub exit_code: Option<i64>,
    pub success: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Checks {
    pub passed: usize,
    pub failed: usize,
    pub incomplete: usize,
    pub commands: Vec<CheckCommand>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Lane {
    pub model: String,
    pub name: String,
    pub session_id: String,
    pub job_id: String,
    pub worktree: PathBuf,
    pub worktree_id: String,
    pub branch: String,
    pub base_commit: String,
    pub status: String,
    pub summary: String,
    pub changed_files: Vec<FileStat>,
    pub changed_files_truncated: bool,
    pub checks: Checks,
    pub duration_s: f64,
    pub timings: Option<crate::timing::Timings>,
    pub usage: Value,
    pub local_runtime: Value,
    pub local_progress: Value,
    pub error: Option<String>,
    /// The lane worktree and its managed branch were removed.
    pub removed: bool,
    /// Job id and finish time the stored diffstat belongs to.
    stats_for: String,
}

/// Durable intent. Unknown application outcomes retain lane material and
/// refuse further mutation until reconciliation can establish what happened.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Recovery {
    pub operation_id: String,
    pub phase: String,
    pub model: String,
    pub result_commit: String,
    pub before_commit: String,
    pub expected_head: String,
    pub expected_ref: String,
    pub index_tree: String,
    pub after_tree: String,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub id: String,
    pub workspace: PathBuf,
    pub task: String,
    pub mode: String,
    pub web: bool,
    pub created_at: f64,
    pub finished_at: Option<f64>,
    /// running | done | needs_review | applied | discarded
    pub state: String,
    pub base: Base,
    pub lanes: Vec<Lane>,
    pub winner: Option<String>,
    pub applied_files: Vec<String>,
    /// Logical outcome is durable, but one or more lane checkouts remain.
    pub cleanup_pending: bool,
    pub recovery: Option<Recovery>,
    /// Cleanup problems and lanes that went missing outside ShadowCode.
    pub notes: Vec<String>,
    /// Runs were added to the scoreboard, or will not be (internal; not in
    /// `to_json`).
    counted: bool,
    /// Bumped by every save; a save expects the revision it loaded
    /// (internal; not in `to_json`).
    revision: u64,
    /// `refresh` decided this comparison's runs count; the next `save` adds
    /// them to the scoreboard in the same transaction (in memory only).
    #[serde(skip)]
    count_runs: bool,
}
impl Record {
    /// The API view: internal bookkeeping fields are left out.
    pub fn to_json(&self) -> Value {
        let mut value = json!(self);
        if let Some(map) = value.as_object_mut() {
            map.remove("counted");
            map.remove("revision");
        }
        for lane in value["lanes"].as_array_mut().into_iter().flatten() {
            if let Some(map) = lane.as_object_mut() {
                map.remove("stats_for");
            }
        }
        value
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct ScoreRow {
    model: String,
    name: String,
    wins: u64,
    runs: u64,
}

/// Another ShadowCode process on this profile saved the comparison after it
/// was loaded here; its version is kept.
#[derive(Debug)]
struct Conflict;
impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "This comparison was changed by another ShadowCode window; reload it and try again",
        )
    }
}
impl std::error::Error for Conflict {}

fn valid_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "Unknown comparison"
    );
    Ok(())
}
fn load(store: &Store, id: &str) -> Result<Record> {
    valid_id(id)?;
    let text = store
        .native_meta(&keys::compare_record(id))?
        .context("Comparison not found")?;
    Ok(serde_json::from_str(&text)?)
}
/// Write a record back in one transaction with everything that depends on
/// it: a new record joins its project's index; runs `refresh` decided to
/// count and a first winner go to the scoreboard. The stored revision must
/// be the one the record was loaded at, so the scoreboard changes exactly
/// once however many processes save the same comparison.
fn save(store: &Store, record: &mut Record) -> Result<()> {
    let key = keys::compare_record(&record.id);
    store.meta_transaction(|meta| {
        let stored: Option<Record> = meta.json(&key)?;
        match &stored {
            Some(stored) if stored.revision != record.revision => return Err(Conflict.into()),
            Some(_) => {}
            None => {
                let index = keys::compare_index(&record.workspace);
                let mut ids: Vec<String> = meta
                    .get(&index)?
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or_default();
                ids.insert(0, record.id.clone());
                ids.truncate(INDEXED);
                meta.set_json(&index, &ids)?;
            }
        }
        let stored_counted = stored.as_ref().is_some_and(|stored| stored.counted);
        if record.count_runs && !stored_counted {
            bump(meta, &record.workspace, &record.lanes, None)?;
        }
        let stored_winner = stored.as_ref().and_then(|stored| stored.winner.as_deref());
        if let Some(winner) = record.winner.as_deref().filter(|_| stored_winner.is_none()) {
            bump(meta, &record.workspace, &record.lanes, Some(winner))?;
        }
        let next = Record {
            revision: record.revision + 1,
            ..record.clone()
        };
        meta.set_json(&key, &next)
    })?;
    record.revision += 1;
    record.count_runs = false;
    Ok(())
}
/// `save` on the blocking pool; returns the saved record.
async fn persist(store: &Arc<Store>, mut record: Record) -> Result<Record> {
    store
        .run(move |store| {
            save(store, &mut record)?;
            Ok(record)
        })
        .await
}
/// `persist` for a refresh-only operation: when another process saved
/// first, its (at least as fresh) version is returned instead.
async fn persist_refreshed(store: &Arc<Store>, mut record: Record) -> Result<Record> {
    store
        .run(move |store| match save(store, &mut record) {
            Err(error) if error.is::<Conflict>() => load(store, &record.id),
            Err(error) => Err(error),
            Ok(()) => Ok(record),
        })
        .await
}
fn index(store: &Store, workspace: &Path) -> Result<Vec<String>> {
    Ok(store
        .native_meta(&keys::compare_index(workspace))?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default())
}
fn scores(store: &Store, workspace: &Path) -> Result<Vec<ScoreRow>> {
    Ok(store
        .native_meta(&keys::compare_scoreboard(workspace))?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default())
}
/// Add one run (no winner) or one win to each lane's model, inside the
/// caller's transaction.
fn bump(
    meta: &MetaTransaction<'_>,
    workspace: &Path,
    lanes: &[Lane],
    winner: Option<&str>,
) -> Result<()> {
    let key = keys::compare_scoreboard(workspace);
    let mut rows: Vec<ScoreRow> = meta
        .get(&key)?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    for lane in lanes {
        let position = match rows.iter().position(|row| row.model == lane.model) {
            Some(position) => position,
            None => {
                rows.push(ScoreRow {
                    model: lane.model.clone(),
                    ..Default::default()
                });
                rows.len() - 1
            }
        };
        let row = &mut rows[position];
        row.name = lane.name.clone();
        if winner.is_none() {
            row.runs += 1;
        } else if winner == Some(lane.model.as_str()) {
            row.wins += 1;
        }
    }
    meta.set_json(&key, &rows)
}

fn active(status: &str) -> bool {
    matches!(status, "" | "queued" | "running" | "paused" | "cancelling")
}

pub(crate) async fn git_with(
    dir: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    cancel: &CancellationToken,
) -> Result<ProcessResult> {
    let mut flags = vec![
        "--no-pager",
        "--literal-pathspecs",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "color.ui=false",
        "-c",
        "core.quotepath=false",
    ];
    // Lane snapshots and diffs run automatically after agent turns: never
    // with the repository's own filter drivers (`crate::git_guard`).
    let guard = crate::git_guard::args_async(dir).await?;
    flags.extend(guard.iter().map(String::as_str));
    flags.extend_from_slice(args);
    let mut spec = ProcessSpec::command("git", &flags, dir.into());
    spec.timeout = Duration::from_secs(300);
    spec.output_limit = 4_000_000;
    for (key, value) in env {
        spec.env.insert((*key).into(), (*value).into());
    }
    process::run(spec, cancel.clone(), None).await
}
pub(crate) async fn git(dir: &Path, args: &[&str], cancel: &CancellationToken) -> Result<String> {
    git_env(dir, args, &[], cancel).await
}
async fn git_env(
    dir: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    cancel: &CancellationToken,
) -> Result<String> {
    let result = git_with(dir, args, env, cancel).await?;
    ensure!(
        result.ok,
        "Git {} failed: {}{}",
        args.first().copied().unwrap_or(""),
        result.stderr.trim(),
        result.stdout.trim()
    );
    ensure!(!result.truncated, "Git output exceeded its limit");
    Ok(result.stdout.trim_end_matches('\n').into())
}

/// Capture HEAD plus the working tree (tracked changes, staged or not, and
/// untracked files that are not ignored) as one commit, using a temporary
/// copy of the index. Only new Git objects are written; the source index and
/// working tree are left exactly as they were.
pub(crate) async fn snapshot(
    scratch: &Path,
    source: &Path,
    cancel: &CancellationToken,
) -> Result<Base> {
    snapshot_for(
        scratch,
        source,
        "comparing models",
        "ShadowCode compare base",
        cancel,
    )
    .await
}
/// `snapshot` for another feature: `action` completes "… before <action>"
/// in its errors and `message` is the base commit's message.
pub(crate) async fn snapshot_for(
    scratch: &Path,
    source: &Path,
    action: &str,
    message: &str,
    cancel: &CancellationToken,
) -> Result<Base> {
    let head = git(source, &["rev-parse", "--verify", "HEAD^{commit}"], cancel)
        .await
        .with_context(|| format!("Make a first commit in this repository before {action}"))?;
    let unmerged = git(source, &["ls-files", "--unmerged", "-z"], cancel).await?;
    ensure!(
        unmerged.is_empty(),
        "Resolve merge conflicts in the project before {action}"
    );
    let real_index = PathBuf::from(
        git(
            source,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
            cancel,
        )
        .await?,
    );
    let temporary = tempfile::tempdir_in(scratch)?;
    let index = temporary.path().join("index");
    let index_text = index.to_str().context("Index path must be UTF-8")?;
    let env = [("GIT_INDEX_FILE", index_text)];
    // Keep staged/force-added entries and sparse paths absent from disk. A
    // failed copy must not silently fall back to HEAD and lose that state.
    if real_index.is_file() {
        let mut original = fs::File::open(&real_index)?;
        let modified = original.metadata()?.modified()?;
        let mut copy = fs::File::create(&index)?;
        std::io::copy(&mut original, &mut copy)?;
        // Git uses the index timestamp to detect racily-clean stat entries.
        // Giving the copy a fresh timestamp can hide same-size working edits.
        copy.set_modified(modified)?;
        drop(copy);
        snapshot_index_flags(source, &env, cancel).await?;
        // Clearing flags rewrites the temporary index. Preserve the source's
        // racy timestamp for other entries before Git examines working bytes.
        fs::File::open(&index)?.set_modified(modified)?;
    } else {
        git_env(source, &["read-tree", "HEAD"], &env, cancel).await?;
    }
    git_env(source, &["add", "--all", "--sparse"], &env, cancel)
        .await
        .context("Could not capture uncommitted work")?;
    let tree = git_env(source, &["write-tree"], &env, cancel).await?;
    let head_tree = git(source, &["rev-parse", "HEAD^{tree}"], cancel).await?;
    if tree == head_tree {
        return Ok(Base {
            commit: head.clone(),
            head,
            included_uncommitted: false,
        });
    }
    let mut args = IDENTITY.to_vec();
    args.extend(["commit-tree", &tree, "-p", &head, "-m", message]);
    let commit = git(source, &args, cancel).await?;
    Ok(Base {
        commit,
        head,
        included_uncommitted: true,
    })
}

/// These flags optimize the user's index, not a complete working-tree capture.
/// Change only the temporary copy. An absent skip-worktree path is sparse
/// content, not a deletion; keep its indexed bytes in the snapshot.
async fn snapshot_index_flags(
    source: &Path,
    env: &[(&str, &str)],
    cancel: &CancellationToken,
) -> Result<()> {
    let entries = git_env(source, &["ls-files", "-v", "-z"], env, cancel).await?;
    let mut assumed = Vec::new();
    let mut present = Vec::new();
    for entry in entries.split('\0').filter(|entry| !entry.is_empty()) {
        let (flag, name) = entry.split_once(' ').context("Invalid Git index entry")?;
        if flag.as_bytes().first().is_some_and(u8::is_ascii_lowercase) {
            assumed.push(name);
        }
        if flag.eq_ignore_ascii_case("S") {
            match fs::symlink_metadata(source.join(name)) {
                Ok(_) => present.push(name),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("Could not inspect sparse working file"),
            }
        }
    }
    for (flag, names) in [
        ("--no-assume-unchanged", assumed),
        ("--no-skip-worktree", present),
    ] {
        // Bound argv size and keep unusual names as literal individual paths.
        for chunk in names.chunks(64) {
            let mut args = vec!["update-index", flag, "--"];
            args.extend_from_slice(chunk);
            git_env(source, &args, env, cancel).await?;
        }
    }
    Ok(())
}

/// Diffstat of a lane against its base: committed and uncommitted tracked
/// changes plus untracked (not ignored) files.
pub(crate) async fn diffstat(
    lane: &Path,
    base: &str,
    cancel: &CancellationToken,
) -> Result<(Vec<FileStat>, bool)> {
    let numstat = git(
        lane,
        &[
            "diff",
            "--numstat",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            base,
            "--",
        ],
        cancel,
    )
    .await?;
    let statuses = git(
        lane,
        &["diff", "--name-status", "-z", "--no-renames", base, "--"],
        cancel,
    )
    .await?;
    let mut kinds = BTreeMap::new();
    let mut tokens = statuses.split('\0').filter(|s| !s.is_empty());
    while let (Some(kind), Some(path)) = (tokens.next(), tokens.next()) {
        kinds.insert(
            path.to_owned(),
            match kind.chars().next() {
                Some('A') => "added",
                Some('D') => "deleted",
                _ => "modified",
            },
        );
    }
    let mut files = Vec::new();
    for entry in numstat.split('\0').filter(|s| !s.is_empty()) {
        let mut fields = entry.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        files.push(FileStat {
            path: path.into(),
            status: kinds.get(path).copied().unwrap_or("modified").into(),
            additions: added.parse().unwrap_or(0),
            deletions: deleted.parse().unwrap_or(0),
            binary: added == "-",
        });
    }
    let untracked = git(
        lane,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        cancel,
    )
    .await?;
    for path in untracked.split('\0').filter(|s| !s.is_empty()) {
        if files.len() >= MAX_FILES {
            return Ok((files, true));
        }
        let (additions, binary) = count_lines(&lane.join(path));
        files.push(FileStat {
            path: path.into(),
            status: "added".into(),
            additions,
            deletions: 0,
            binary,
        });
    }
    let truncated = files.len() > MAX_FILES;
    files.truncate(MAX_FILES);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((files, truncated))
}
fn count_lines(path: &Path) -> (u64, bool) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return (0, false);
    };
    if !meta.is_file() || meta.len() > COUNT_LIMIT {
        return (0, !meta.is_file());
    }
    let mut bytes = Vec::new();
    if fs::File::open(path)
        .and_then(|file| file.take(COUNT_LIMIT).read_to_end(&mut bytes))
        .is_err()
    {
        return (0, false);
    }
    if bytes.contains(&0) {
        return (0, true);
    }
    let lines = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    (
        lines + u64::from(!bytes.is_empty() && !bytes.ends_with(b"\n")),
        false,
    )
}

fn checks_of(engine: &Engine, job: &Job) -> Checks {
    let verification = job
        .result
        .as_ref()
        .map(|result| result["verification"].clone())
        .filter(|v| !v.is_null())
        .or_else(|| {
            engine
                .store()
                .last_task_event(&job.task_id, "verification.summary")
                .ok()
                .flatten()
                .map(|event| event["payload"].clone())
        })
        .unwrap_or(Value::Null);
    checks_from(&verification)
}

fn checks_from(verification: &Value) -> Checks {
    let latest = crate::verification::latest_checks(
        verification["commands"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    );
    let commands: Vec<CheckCommand> = latest
        .iter()
        .map(|command| CheckCommand {
            state: command["state"].as_str().unwrap_or("not_run").into(),
            command: command["command"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| command["command"].to_string()),
            exit_code: command["exit_code"].as_i64(),
            success: command["state"] == "passed" && command["success"] == true,
        })
        .collect();
    Checks {
        passed: commands.iter().filter(|c| c.success).count(),
        failed: commands.iter().filter(|c| c.state == "failed").count(),
        incomplete: commands
            .iter()
            .filter(|c| !matches!(c.state.as_str(), "passed" | "failed"))
            .count(),
        commands,
    }
}

/// Bring lane status, checks and diffstat up to date; move a running compare
/// to `done` once every lane's latest job has finished.
async fn refresh(engine: &Engine, record: &mut Record, cancel: &CancellationToken) -> Result<()> {
    let store = engine.store();
    for lane in &mut record.lanes {
        if lane.removed {
            continue;
        }
        // Follow-up turns in a lane conversation update the lane.
        let latest = store
            .session_jobs(&lane.session_id, 1)?
            .pop()
            .and_then(|job| job["id"].as_str().map(str::to_owned))
            .unwrap_or_else(|| lane.job_id.clone());
        let Some(job) = engine.job(&latest)? else {
            lane.status = "failed".into();
            lane.error = Some("The lane's task record is missing".into());
            continue;
        };
        lane.job_id = job.id.clone();
        lane.status = job.status.clone();
        lane.summary = job.summary.clone();
        // A follow-up owns its runtime evidence; never retain the prior turn's.
        lane.local_runtime = store
            .last_task_event(&job.task_id, "local.runtime_ready")?
            .map(|event| event["payload"].clone())
            .unwrap_or(Value::Null);
        lane.local_progress = if lane.local_runtime.is_null() {
            store
                .last_task_event(&job.task_id, "local.runtime_progress")?
                .map(|event| event["payload"].clone())
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        lane.usage = json!({
            "prompt_tokens": job.usage.prompt_tokens,
            "completion_tokens": job.usage.completion_tokens,
            "total_tokens": job.usage.total_tokens,
            "estimated": job.usage_is_estimated,
        });
        lane.duration_s = (job
            .timings
            .as_ref()
            .map(|timings| timings.active_seconds.unwrap_or(0.0))
            .unwrap_or_else(|| {
                (job.finished_at.unwrap_or_else(crate::now) - job.started_at).max(0.0)
            })
            * 10.0)
            .round()
            / 10.0;
        lane.timings = job.timings.clone();
        lane.checks = checks_of(engine, &job);
        lane.error = matches!(
            job.status.as_str(),
            "failed" | "limit_reached" | "interrupted"
        )
        .then(|| job.summary.clone());
        if !lane.worktree.is_dir() {
            let note = format!(
                "{}: its worktree {} was deleted outside ShadowCode",
                lane.name,
                lane.worktree.display()
            );
            lane.error = Some(note.clone());
            if !record.notes.contains(&note) {
                record.notes.push(note);
            }
            continue;
        }
        let key = format!("{}:{:?}", job.id, job.finished_at);
        if active(&job.status) || lane.stats_for != key {
            match diffstat(&lane.worktree, &lane.base_commit, cancel).await {
                Ok((files, truncated)) => {
                    lane.changed_files = files;
                    lane.changed_files_truncated = truncated;
                    if !active(&job.status) {
                        lane.stats_for = key;
                    }
                }
                Err(error) => lane.error = Some(format!("Could not read lane changes: {error:#}")),
            }
        }
    }
    if record.state == "running" && record.lanes.iter().all(|lane| !active(&lane.status)) {
        record.state = "done".into();
        record.finished_at = Some(crate::now());
    }
    if !record.counted && record.state != "running" && record.state != "discarded" {
        // Added to the scoreboard by the save that stores this decision.
        record.counted = true;
        record.count_runs = true;
    }
    Ok(())
}

pub fn parse_mode(mode: &str) -> Result<(&'static str, &'static str)> {
    Ok(match mode {
        "" | "code" => ("code", "coder"),
        "plan" => ("plan", "planner"),
        "ask" => ("review", "reviewer"),
        other => bail!("Unknown compare mode '{other}'; use code, plan or ask"),
    })
}

/// Validate the lineup before anything is created. Returns (id, model) pairs.
fn resolve_models(
    engine: &Engine,
    cfg: &Config,
    ids: &[String],
) -> Result<Vec<(String, config::ModelConfig)>> {
    ensure!((2..=3).contains(&ids.len()), "Compare needs 2 or 3 models");
    for (i, id) in ids.iter().enumerate() {
        ensure!(
            !id.trim().is_empty() && id.len() <= 1024,
            "Choose a model for every lane"
        );
        ensure!(
            !ids[..i].contains(id),
            "Choose different models; {id} is listed twice"
        );
    }
    let store = engine.store();
    let mut resolved: Vec<(String, config::ModelConfig)> = Vec::new();
    for id in ids {
        crate::local_engine::precheck_job(&cfg.local_engine, id, 0)?;
        crate::openrouter::precheck(engine.paths(), id, cfg.offline())?;
        let model = model_registry::resolve(&store, id, &cfg.model)
            .with_context(|| format!("Could not use {id}"))?;
        ensure!(
            !cfg.offline() || config::runs_on_this_computer(&model),
            "Offline mode: {} does not run on this computer; compare local models only",
            model.name
        );
        ensure!(
            model.provider != "mock",
            "Choose a local or compatible model for every lane"
        );
        if let Some(vendor) = crate::cli_agent::Vendor::from_provider(&model.provider) {
            ensure!(
                cfg.cli_agents.vendor_enabled(vendor),
                "{} is disabled in Settings → Advanced",
                vendor.label()
            );
        }
        ensure!(
            !resolved
                .iter()
                .any(|(_, other)| other.default == model.default),
            "{id} is the same model as another lane; choose different models"
        );
        resolved.push((id.clone(), model));
    }
    Ok(resolved)
}

async fn repository_root(workspace: &Path, cancel: &CancellationToken) -> Result<()> {
    repository_root_for(workspace, "Compare needs", "comparing models", cancel).await
}
/// `workspace` is a Git repository's root: "<needs> a Git repository" and
/// "Open the repository root before <action>" otherwise.
pub(crate) async fn repository_root_for(
    workspace: &Path,
    needs: &str,
    action: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    let top = git(workspace, &["rev-parse", "--show-toplevel"], cancel)
        .await
        .with_context(|| format!("{needs} a Git repository"))?;
    ensure!(
        Path::new(&top).canonicalize()? == workspace,
        "Open the repository root before {action}"
    );
    Ok(())
}

pub(crate) fn set_trust(engine: &Engine, lanes: &[PathBuf], trusted: bool) -> Result<()> {
    Config::update(engine.paths(), |cfg| {
        if trusted {
            for lane in lanes {
                cfg.grant_trust(lane);
            }
        } else {
            cfg.trusted_workspaces.retain(|stored| {
                !lanes
                    .iter()
                    .any(|lane| Path::new(stored.trim()) == lane.as_path())
            });
        }
        Ok(())
    })?;
    Ok(())
}

/// Remove lane worktrees and their managed branches; returns notes.
async fn remove_lanes(engine: &Engine, record: &mut Record) -> Vec<String> {
    let mut notes = Vec::new();
    let mut removed = Vec::new();
    for lane in &mut record.lanes {
        if lane.removed || lane.worktree_id.is_empty() {
            continue;
        }
        // Recheck ownership at every destructive boundary, including cleanup
        // retries. A recorded cancellation request is not process completion.
        let reservation = (|| -> Result<_> {
            if let Some(job) = engine.job(&lane.job_id)? {
                ensure!(!active(&job.status), "Lane task is still stopping");
            }
            Ok((
                engine.reserve_workspace(&lane.worktree)?,
                engine.background().reserve_idle_workspace(&lane.worktree)?,
            ))
        })();
        let _reservation = match reservation {
            Ok(reservation) => reservation,
            Err(error) => {
                notes.push(format!(
                    "{}: cleanup pending; worktree was kept: {error:#}",
                    lane.name
                ));
                continue;
            }
        };
        match worktrees::dispose(
            engine.paths(),
            &record.workspace,
            &lane.worktree_id,
            CancellationToken::new(),
        )
        .await
        {
            Ok(note) => {
                lane.removed = true;
                removed.push(lane.worktree.clone());
                notes.extend(note.map(|note| format!("{}: {note}", lane.name)));
            }
            Err(error) => notes.push(format!(
                "{}: worktree {} was kept: {error:#}",
                lane.name,
                lane.worktree.display()
            )),
        }
    }
    record.cleanup_pending = record.lanes.iter().any(|lane| !lane.removed);
    if let Err(error) = set_trust(engine, &removed, false) {
        notes.push(format!("Could not update trusted projects: {error:#}"));
    }
    notes
}

/// Cancel active lane jobs and wait (bounded) for them to stop.
async fn stop_lanes(engine: &Engine, record: &Record, except: Option<&str>) -> Result<()> {
    for lane in &record.lanes {
        if lane.removed || except == Some(lane.model.as_str()) || !active(&lane.status) {
            continue;
        }
        engine.request_cancel(&lane.job_id)?;
    }
    for lane in &record.lanes {
        if lane.removed || except == Some(lane.model.as_str()) || !active(&lane.status) {
            continue;
        }
        tokio::time::timeout(Duration::from_secs(60), engine.wait(&lane.job_id))
            .await
            .with_context(|| format!("{} is still stopping; try again shortly", lane.name))??;
    }
    Ok(())
}

pub(crate) struct StartOptions<'a> {
    pub workspace: PathBuf,
    pub task: String,
    pub models: Vec<String>,
    pub mode: String,
    pub web: bool,
    pub owner: Option<&'a JobOwner>,
}

fn ensure_no_unsaved_editor_drafts(store: &Store, workspace: &Path) -> Result<()> {
    let paths = store.unsaved_editor_draft_paths(workspace)?;
    ensure!(
        paths.is_empty(),
        "Save or discard open editor drafts before comparing: {}",
        paths.join(", ")
    );
    Ok(())
}

pub(crate) async fn start(engine: &Engine, options: StartOptions<'_>) -> Result<Record> {
    let cancel = CancellationToken::new();
    let workspace = Workspace::open(&options.workspace)?.path;
    let cfg = Config::load(engine.paths(), Some(&workspace))?;
    ensure!(
        cfg.is_trusted(&workspace),
        "Trust this project before comparing models"
    );
    let task = options.task.trim().to_owned();
    ensure!(
        !task.is_empty() && task.len() <= 128_000,
        "Task must contain between 1 and 128000 bytes"
    );
    let (mode, purpose) = parse_mode(&options.mode)?;
    let models = resolve_models(engine, &cfg, &options.models)?;
    repository_root(&workspace, &cancel).await?;
    let lock = project_lock(&workspace)?;
    let _guard = lock.lock().await;
    let _ownership = mutation_lock(&workspace, &cancel).await?;
    ensure_no_unsaved_editor_drafts(&engine.store(), &workspace)?;
    let base = snapshot(&engine.paths().data, &workspace, &cancel).await?;
    let mut record = Record {
        id: crate::id(),
        workspace: workspace.clone(),
        task: task.clone(),
        mode: if mode == "review" { "ask" } else { mode }.into(),
        web: options.web,
        created_at: crate::now(),
        state: "running".into(),
        base: base.clone(),
        ..Default::default()
    };
    // Create every checkout before any job starts, so a failure leaves
    // nothing running and removes what was created.
    let created: Result<()> = async {
        for (id, model) in &models {
            let checkout =
                worktrees::create(engine.paths(), &workspace, &base.commit, cancel.clone()).await?;
            record.lanes.push(Lane {
                model: id.clone(),
                name: if model.name.is_empty() {
                    id.clone()
                } else {
                    model.name.clone()
                },
                worktree: checkout.path.canonicalize()?,
                worktree_id: checkout.id,
                branch: checkout.branch,
                base_commit: base.commit.clone(),
                status: "queued".into(),
                usage: json!({}),
                ..Default::default()
            });
        }
        let lanes: Vec<PathBuf> = record.lanes.iter().map(|l| l.worktree.clone()).collect();
        set_trust(engine, &lanes, true)
    }
    .await;
    if let Err(error) = created {
        remove_lanes(engine, &mut record).await;
        return Err(error);
    }
    let store = engine.store();
    let limit = Some(cfg.permissions.level.clone());
    let mut failure = None;
    for (lane, (id, model)) in record.lanes.iter_mut().zip(&models) {
        let started: Result<Job> = async {
            let session = store.create_session(
                &lane.worktree,
                &model.default,
                &format!("Compare · {}", lane.name),
            )?;
            let sid = session["id"]
                .as_str()
                .context("Missing session ID")?
                .to_owned();
            lane.session_id = sid.clone();
            store.set_session_meta(&sid, keys::COMPARE_ID, &record.id)?;
            store.set_session_meta(&sid, keys::COMPARE_LANE, id)?;
            store.set_session_meta(&sid, keys::EXECUTION_TARGET, id)?;
            engine
                .start_consented_owned(
                    StartRequest {
                        workspace: lane.worktree.clone(),
                        task: task.clone(),
                        session_id: Some(sid),
                        model: Some(model.clone()),
                        mode: mode.into(),
                        queue: false,
                        images: Vec::new(),
                        web: options.web,
                    },
                    purpose,
                    limit.clone(),
                    options.owner,
                    false,
                )
                .await
        }
        .await;
        match started {
            Ok(job) => {
                lane.job_id = job.id;
                lane.status = job.status;
            }
            Err(error) => {
                failure = Some(error.context(format!("Could not start {}", lane.name)));
                break;
            }
        }
    }
    if let Some(error) = failure {
        for lane in &record.lanes {
            if !lane.job_id.is_empty() {
                let _ = engine.cancel(&lane.job_id).await;
            }
        }
        remove_lanes(engine, &mut record).await;
        for lane in &record.lanes {
            if !lane.session_id.is_empty() {
                let _ = engine.delete_session(&lane.session_id);
            }
        }
        return Err(error);
    }
    // Also lists the new record in its project's index.
    persist(&store, record).await
}

pub async fn get(engine: &Engine, id: &str) -> Result<Record> {
    let store = engine.store();
    let workspace = load(&store, id)?.workspace;
    let lock = project_lock(&workspace)?;
    let _guard = lock.lock().await;
    let mut record = load(&store, id)?;
    refresh(engine, &mut record, &CancellationToken::new()).await?;
    let mut record = persist_refreshed(&store, record).await?;
    // Content reads do not hold the project metadata lock. The response is a
    // point-in-time view; Keep repeats validation at its mutation boundary.
    drop(_guard);
    for lane in &mut record.lanes {
        if !lane.removed && !active(&lane.status) {
            if let Some(job) = engine.job(&lane.job_id)? {
                lane.checks = checks_from(&crate::verification::current(engine, &job).await?);
            }
        }
    }
    Ok(record)
}

pub async fn list(engine: &Engine, workspace: &Path) -> Result<Vec<Record>> {
    let workspace = Workspace::open(workspace)?.path;
    let lock = project_lock(&workspace)?;
    let _guard = lock.lock().await;
    let store = engine.store();
    let mut records = Vec::new();
    for id in index(&store, &workspace)?.iter().take(LISTED) {
        let Ok(mut record) = load(&store, id) else {
            continue;
        };
        if matches!(record.state.as_str(), "running" | "done") {
            refresh(engine, &mut record, &CancellationToken::new()).await?;
            record = persist_refreshed(&store, record).await?;
        }
        records.push(record);
    }
    records.sort_by(|a, b| b.created_at.total_cmp(&a.created_at));
    Ok(records)
}

pub async fn scoreboard(engine: &Engine, workspace: &Path) -> Result<Value> {
    // Listing first moves finished comparisons to `done` and counts them.
    list(engine, workspace).await?;
    let workspace = Workspace::open(workspace)?.path;
    let mut rows = scores(&engine.store(), &workspace)?;
    rows.sort_by(|a, b| {
        b.wins
            .cmp(&a.wins)
            .then(b.runs.cmp(&a.runs))
            .then(a.model.cmp(&b.model))
    });
    Ok(json!({"workspace": workspace, "rows": rows}))
}

/// Paths named in `git apply` errors (conflicting files).
pub(crate) fn conflicting_paths(stderr: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in stderr.lines() {
        let Some(rest) = line.strip_prefix("error: ") else {
            continue;
        };
        let path = if let Some(rest) = rest.strip_prefix("patch failed: ") {
            rest.rsplit_once(':').map(|(path, _)| path).unwrap_or(rest)
        } else if let Some((path, _)) = rest.split_once(": ") {
            path
        } else {
            continue;
        };
        if !path.is_empty() && !paths.iter().any(|p| p == path) {
            paths.push(path.to_owned());
        }
    }
    paths
}

/// Commit everything in a disposable checkout (tracked and untracked, not
/// ignored) on its own managed branch — never one of the user's — and list
/// the files that differ from `base`. Returns (head commit, files).
pub(crate) async fn commit_checkout(
    checkout: &Path,
    base: &str,
    message: &str,
    cancel: &CancellationToken,
) -> Result<(String, Vec<String>)> {
    git(checkout, &["add", "--all"], cancel).await?;
    let staged = git_with(
        checkout,
        &["diff", "--cached", "--quiet", "--no-ext-diff"],
        &[],
        cancel,
    )
    .await?;
    if !staged.ok {
        let mut args = IDENTITY.to_vec();
        args.extend(["commit", "--quiet", "--no-verify", "-m", message]);
        git(checkout, &args, cancel).await?;
    }
    let head = git(checkout, &["rev-parse", "--verify", "HEAD"], cancel).await?;
    let files: Vec<String> = git(
        checkout,
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            base,
            &head,
            "--",
        ],
        cancel,
    )
    .await?
    .split('\0')
    .filter(|s| !s.is_empty())
    .map(str::to_owned)
    .collect();
    Ok((head, files))
}

/// `git apply --check` refused a result; nothing was written.
#[derive(Clone, Debug, Default)]
pub(crate) struct Refused {
    /// Files Git named as conflicting.
    pub conflicts: Vec<String>,
    /// Git's own words, for when it named no file.
    pub detail: String,
}
impl Refused {
    /// " in a, b" or " (Git's message)".
    pub fn described(&self) -> String {
        if self.conflicts.is_empty() {
            format!(" ({})", self.detail)
        } else {
            format!(" in {}", self.conflicts.join(", "))
        }
    }
}

/// Apply `base..head` of `checkout` to `target`'s working tree (never the
/// index, never a commit), so the user reviews it in the normal Changes
/// view. `git apply --check` runs first; when it refuses, nothing is written
/// and the refusal is returned.
pub(crate) async fn apply_checkout(
    scratch: &Path,
    checkout: &Path,
    target: &Path,
    base: &str,
    head: &str,
    cancel: &CancellationToken,
) -> Result<Option<Refused>> {
    let temporary = tempfile::tempdir_in(scratch)?;
    let patch = temporary.path().join("result.patch");
    let output = format!(
        "--output={}",
        patch.to_str().context("Patch path must be UTF-8")?
    );
    git(
        checkout,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            &output,
            base,
            head,
            "--",
        ],
        cancel,
    )
    .await?;
    let patch_path = patch.to_str().context("Patch path must be UTF-8")?;
    let check = git_with(
        target,
        &[
            "apply",
            "--check",
            "--binary",
            "--whitespace=nowarn",
            "--",
            patch_path,
        ],
        &[],
        cancel,
    )
    .await?;
    if !check.ok {
        return Ok(Some(Refused {
            conflicts: conflicting_paths(&check.stderr),
            detail: check.stderr.trim().to_owned(),
        }));
    }
    git(
        target,
        &["apply", "--binary", "--whitespace=nowarn", "--", patch_path],
        cancel,
    )
    .await
    .context("Applying the changes failed; review the project's Changes")?;
    Ok(None)
}

/// Compute expected postimages in an isolated index; no source files or real
/// index are changed. A preflight conflict has no expected postimage.
async fn projected_tree(
    scratch: &Path,
    lane: &Lane,
    head: &str,
    before: &str,
    cancel: &CancellationToken,
) -> Result<String> {
    let temp = tempfile::tempdir_in(scratch)?;
    let patch = temp.path().join("projection.patch");
    let output = format!("--output={}", patch.display());
    git(
        &lane.worktree,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            &output,
            &lane.base_commit,
            head,
            "--",
        ],
        cancel,
    )
    .await?;
    let index = temp.path().join("index");
    let env = [(
        "GIT_INDEX_FILE",
        index.to_str().context("Index path must be UTF-8")?,
    )];
    git_env(&lane.worktree, &["read-tree", before], &env, cancel).await?;
    let applied = git_with(
        &lane.worktree,
        &[
            "apply",
            "--cached",
            "--binary",
            "--whitespace=nowarn",
            "--",
            patch.to_str().context("Patch path must be UTF-8")?,
        ],
        &env,
        cancel,
    )
    .await?;
    if !applied.ok {
        return Ok(String::new());
    }
    git_env(&lane.worktree, &["write-tree"], &env, cancel).await
}

async fn index_tree(
    scratch: &Path,
    workspace: &Path,
    cancel: &CancellationToken,
) -> Result<String> {
    let original = git(
        workspace,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        cancel,
    )
    .await?;
    let temp = tempfile::tempdir_in(scratch)?;
    let copy = temp.path().join("index");
    fs::copy(original, &copy).context("Could not preserve index identity for recovery")?;
    git_env(
        workspace,
        &["write-tree"],
        &[(
            "GIT_INDEX_FILE",
            copy.to_str().context("Index path must be UTF-8")?,
        )],
        cancel,
    )
    .await
}

/// Reconcile only exact recorded states. Never apply, revert, or remove files.
pub async fn recover(engine: &Engine, id: &str) -> Result<Record> {
    let store = engine.store();
    let workspace = load(&store, id)?.workspace;
    let lock = project_lock(&workspace)?;
    let _guard = lock.lock().await;
    let mut record = load(&store, id)?;
    let _ownership = mutation_lock(&record.workspace, &CancellationToken::new()).await?;
    if record.state != "needs_review" {
        return Ok(record);
    }
    let recovery = record
        .recovery
        .clone()
        .context("Recovery evidence is unavailable")?;
    ensure!(
        !recovery.after_tree.is_empty()
            && !recovery.index_tree.is_empty()
            && !recovery.expected_ref.is_empty(),
        "This operation lacks complete recovery evidence; copies are preserved for manual review"
    );
    let _reservation = engine.reserve_workspace(&record.workspace)?;
    let cancel = CancellationToken::new();
    let head = git(&record.workspace, &["rev-parse", "HEAD"], &cancel).await?;
    let reference = git(
        &record.workspace,
        &["rev-parse", "--symbolic-full-name", "HEAD"],
        &cancel,
    )
    .await?;
    ensure!(head == recovery.expected_head && reference == recovery.expected_ref && index_tree(&engine.paths().data, &record.workspace, &cancel).await? == recovery.index_tree,
        "Project branch, HEAD or staged content changed; recovery requires review and no files were changed");
    let current = snapshot_for(
        &engine.paths().data,
        &record.workspace,
        "assessing recovery",
        "ShadowCode recovery assessment",
        &cancel,
    )
    .await?;
    let current_tree = git(
        &record.workspace,
        &["rev-parse", &format!("{}^{{tree}}", current.commit)],
        &cancel,
    )
    .await?;
    let before_tree = git(
        &record.workspace,
        &["rev-parse", &format!("{}^{{tree}}", recovery.before_commit)],
        &cancel,
    )
    .await?;
    if current_tree == recovery.after_tree {
        record.state = "applied".into();
        record.winner = Some(recovery.model);
        record.applied_files = recovery.paths;
        record.cleanup_pending = record.lanes.iter().any(|lane| !lane.removed);
        record.finished_at.get_or_insert_with(crate::now);
        record.recovery.as_mut().unwrap().phase = "applied".into();
    } else if current_tree == before_tree {
        record.state = "done".into();
        record.recovery.as_mut().unwrap().phase = "not_applied".into();
    } else {
        bail!("Current files match neither the recorded preimage nor the expected result. Recovery requires review; no files or copies were changed");
    }
    persist(&store, record).await
}

pub async fn keep(engine: &Engine, id: &str, model: &str) -> Result<Record> {
    keep_reviewed(engine, id, model, false).await
}

pub async fn keep_reviewed(
    engine: &Engine,
    id: &str,
    model: &str,
    accept_unverified: bool,
) -> Result<Record> {
    let cancel = CancellationToken::new();
    let store = engine.store();
    let workspace = load(&store, id)?.workspace;
    let lock = project_lock(&workspace)?;
    let _guard = lock.lock().await;
    let mut record = load(&store, id)?;
    let _ownership = mutation_lock(&record.workspace, &CancellationToken::new()).await?;
    ensure!(
        matches!(record.state.as_str(), "running" | "done"),
        "This comparison was already {}",
        record.state
    );
    refresh(engine, &mut record, &cancel).await?;
    let lane = record
        .lanes
        .iter()
        .find(|lane| lane.model == model)
        .cloned()
        .with_context(|| format!("{model} is not part of this comparison"))?;
    ensure!(
        !active(&lane.status),
        "{} is still working; wait for it to finish or cancel it first",
        lane.name
    );
    ensure!(
        !lane.removed && lane.worktree.is_dir(),
        "{}'s worktree was deleted outside ShadowCode; its result cannot be kept",
        lane.name
    );
    // Keep the selected lane stable against new application-owned tasks
    // while validating its evidence and capturing the immutable result.
    let _lane_reservation = engine.reserve_workspace(&lane.worktree)?;
    let _lane_background = engine.background().reserve_idle_workspace(&lane.worktree)?;
    worktrees::validate_task_checkout(
        engine.paths(),
        &record.workspace,
        &lane.worktree_id,
        &lane.worktree,
        &lane.base_commit,
        &lane.branch,
        &cancel,
    )
    .await?;
    let mut acceptance_checks = lane.checks.clone();
    if let Some(job) = engine.job(&lane.job_id)? {
        let current = checks_from(&crate::verification::current(engine, &job).await?);
        acceptance_checks = current.clone();
        if let Some(selected) = record.lanes.iter_mut().find(|item| item.model == model) {
            selected.checks = current.clone();
        }
        if current.incomplete > 0 && !accept_unverified {
            persist_refreshed(&store, record).await?;
            bail!("This result no longer has current verification. Review the lane and rerun its checks, or explicitly choose Keep without current checks. No project changes were applied.");
        }
        if current.incomplete > 0 {
            record.notes.push(format!(
                "{} kept after explicit review without current verification.",
                lane.name
            ));
        }
    }
    // No agent task may run in the source while its working tree changes.
    let _reservation = engine.reserve_workspace(&record.workspace)?;
    let _source_background = engine
        .background()
        .reserve_idle_workspace(&record.workspace)?;
    // Commit the lane's result on its managed branch (never the user's).
    let (head, files) = commit_checkout(
        &lane.worktree,
        &lane.base_commit,
        &format!("ShadowCode compare result: {}", lane.name),
        &cancel,
    )
    .await?;
    ensure!(
        !files.is_empty() || lane.status == "completed",
        "{} finished as {} without changes; there is nothing to keep",
        lane.name,
        lane.status
    );
    if !files.is_empty() {
        let before = snapshot_for(
            &engine.paths().data,
            &record.workspace,
            "keeping a result",
            "ShadowCode Compare recovery preimage",
            &cancel,
        )
        .await?;
        // Pin preimages against Git garbage collection. Never move HEAD or
        // the user's branch/index. Retention is deliberately conservative.
        let reference = format!("refs/shadowcode/recovery/{}", record.id);
        git(
            &record.workspace,
            &["update-ref", &reference, &before.commit],
            &cancel,
        )
        .await?;
        let after_tree =
            projected_tree(&engine.paths().data, &lane, &head, &before.commit, &cancel).await?;
        let index_tree = index_tree(&engine.paths().data, &record.workspace, &cancel).await?;
        let expected_ref = git(
            &record.workspace,
            &["rev-parse", "--symbolic-full-name", "HEAD"],
            &cancel,
        )
        .await?;
        record.recovery = Some(Recovery {
            operation_id: uuid::Uuid::new_v4().simple().to_string(),
            phase: "applying".into(),
            model: lane.model.clone(),
            result_commit: head.clone(),
            before_commit: before.commit,
            expected_head: before.head,
            expected_ref,
            index_tree,
            after_tree,
            paths: files.clone(),
        });
        record.state = "needs_review".into();
        record = persist(&store, record).await?;
        // Working tree only: no --index, no commit.
        let refused = apply_checkout(
            &engine.paths().data,
            &lane.worktree,
            &record.workspace,
            &lane.base_commit,
            &head,
            &cancel,
        )
        .await?;
        if let Some(refused) = refused {
            // Preflight refusal is an observed no-write outcome, unlike an
            // interrupted or failed apply whose result must be reconciled.
            record.state = "done".into();
            if let Some(recovery) = &mut record.recovery {
                recovery.phase = "not_applied".into();
            }
            persist(&store, record).await?;
            bail!(
                "{}'s changes no longer apply: the project changed since the comparison started{}. Nothing was changed and every lane is kept; update or revert those files, then keep again.",
                lane.name,
                refused.described()
            );
        }
    }
    if let Some(recovery) = &mut record.recovery {
        recovery.phase = "applied".into();
    }
    record.winner = Some(lane.model.clone());
    record.applied_files = files;
    record.state = "applied".into();
    record.finished_at.get_or_insert_with(crate::now);
    if let Some(selected) = record.lanes.iter_mut().find(|item| item.model == model) {
        selected.checks = acceptance_checks;
    }
    record.cleanup_pending = record.lanes.iter().any(|lane| !lane.removed);
    // Durably record the outcome and score before deleting recoverable lanes.
    // A failed save leaves all lane material intact. The earlier apply/save
    // crash window still requires the operation journal.
    record = persist(&store, record).await?;
    drop(_lane_background);
    drop(_lane_reservation);
    if let Err(error) = stop_lanes(engine, &record, Some(model)).await {
        record.notes.push(format!("Cleanup pending: {error:#}"));
        return persist(&store, record).await;
    }
    let notes = remove_lanes(engine, &mut record).await;
    record.notes.extend(notes);
    persist(&store, record).await
}

pub async fn discard(engine: &Engine, id: &str) -> Result<Record> {
    let cancel = CancellationToken::new();
    let store = engine.store();
    let workspace = load(&store, id)?.workspace;
    let lock = project_lock(&workspace)?;
    let _guard = lock.lock().await;
    let mut record = load(&store, id)?;
    let _ownership = mutation_lock(&record.workspace, &CancellationToken::new()).await?;
    ensure!(record.state != "needs_review", "An interrupted Keep requires recovery review. Project files and lane copies have been preserved; cleanup is blocked.");
    if matches!(record.state.as_str(), "applied" | "discarded") {
        // Retry cleanup of anything a previous attempt left behind.
        if record.lanes.iter().any(|lane| !lane.removed) {
            let notes = remove_lanes(engine, &mut record).await;
            record.notes.extend(notes);
            return persist(&store, record).await;
        }
        return Ok(record);
    }
    refresh(engine, &mut record, &cancel).await?;
    // A comparison discarded before it finished does not count as a run.
    if record.state == "running" {
        record.counted = true;
    }
    stop_lanes(engine, &record, None).await?;
    refresh(engine, &mut record, &cancel).await?;
    record.state = "discarded".into();
    record.finished_at.get_or_insert_with(crate::now);
    record.cleanup_pending = record.lanes.iter().any(|lane| !lane.removed);
    record = persist(&store, record).await?;
    let notes = remove_lanes(engine, &mut record).await;
    record.notes.extend(notes);
    persist(&store, record).await
}

pub async fn cancel(engine: &Engine, id: &str) -> Result<Record> {
    let store = engine.store();
    let workspace = load(&store, id)?.workspace;
    let lock = project_lock(&workspace)?;
    let _guard = lock.lock().await;
    let mut record = load(&store, id)?;
    for lane in &record.lanes {
        if !lane.removed && !lane.job_id.is_empty() && active(&lane.status) {
            engine.request_cancel(&lane.job_id)?;
        }
    }
    refresh(engine, &mut record, &CancellationToken::new()).await?;
    persist_refreshed(&store, record).await
}

/// The compare a session belongs to, for session views.
pub fn session_tags(store: &Store, session_id: &str) -> Result<(Option<String>, Option<String>)> {
    Ok((
        store.session_meta(session_id, keys::COMPARE_ID)?,
        store.session_meta(session_id, keys::COMPARE_LANE)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unrelated_project_metadata_remains_available() {
        let a = project_lock(Path::new("/fixture/project-a")).unwrap();
        let same = project_lock(Path::new("/fixture/project-a")).unwrap();
        assert!(Arc::ptr_eq(&a, &same));
        let held = a.lock().await;
        let b = project_lock(Path::new("/fixture/project-b")).unwrap();
        let independent = tokio::time::timeout(Duration::from_millis(100), b.lock()).await;
        assert!(independent.is_ok());
        assert!(same.try_lock().is_err());
        drop(held);
        assert!(same.try_lock().is_ok());
    }

    #[tokio::test]
    async fn workspace_mutations_share_compare_repository_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        git(dir.path(), &["init", "-q"], &cancel).await.unwrap();

        let compare = mutation_lock(dir.path(), &cancel).await.unwrap();
        let blocked = workspace_mutation_lock(dir.path(), &cancel).await;
        assert!(
            blocked.is_err(),
            "a draft write must not race an active Compare snapshot"
        );

        drop(compare);
        assert!(workspace_mutation_lock(dir.path(), &cancel)
            .await
            .unwrap()
            .is_some());

        let standalone = tempfile::tempdir().unwrap();
        assert!(workspace_mutation_lock(standalone.path(), &cancel)
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn apply_errors_name_conflicting_files() {
        let stderr = "error: patch failed: src/a.txt:1\nerror: src/a.txt: patch does not apply\nerror: new.txt: already exists in working directory\n";
        assert_eq!(conflicting_paths(stderr), vec!["src/a.txt", "new.txt"]);
    }

    #[test]
    fn modes_map_to_engine_modes() {
        assert_eq!(parse_mode("ask").unwrap(), ("review", "reviewer"));
        assert_eq!(parse_mode("").unwrap(), ("code", "coder"));
        assert!(parse_mode("command").is_err());
    }

    #[test]
    fn compare_blocks_on_pending_editor_draft() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("store.sqlite")).unwrap();
        let workspace = dir.path().join("project");
        ensure_no_unsaved_editor_drafts(&store, &workspace).unwrap();

        let base = "saved bytes\n";
        let draft = store
            .put_editor_draft(
                &workspace,
                "src/main.rs",
                base,
                "new unsaved bytes\n",
                &crate::workspace::hash(base.as_bytes()),
                "missing",
            )
            .unwrap();
        let error = ensure_no_unsaved_editor_drafts(&store, &workspace).unwrap_err();
        assert!(error.to_string().contains("src/main.rs"));
        assert!(!error.to_string().contains("new unsaved bytes"));

        store
            .delete_editor_draft(&workspace, "src/main.rs", &draft.revision)
            .unwrap();
        ensure_no_unsaved_editor_drafts(&store, &workspace).unwrap();
    }

    fn record(workspace: &Path) -> Record {
        let lane = |model: &str| Lane {
            model: model.into(),
            name: model.to_uppercase(),
            ..Default::default()
        };
        Record {
            id: crate::id(),
            workspace: workspace.into(),
            state: "running".into(),
            lanes: vec![lane("alpha"), lane("beta")],
            ..Default::default()
        }
    }
    fn board(store: &Store, workspace: &Path) -> BTreeMap<String, (u64, u64)> {
        scores(store, workspace)
            .unwrap()
            .into_iter()
            .map(|row| (row.model, (row.wins, row.runs)))
            .collect()
    }

    /// Comparisons finishing at once, saved through separate connections
    /// (as a second ShadowCode process would) and a shared one: every
    /// record reaches the project index and every run is counted.
    #[test]
    fn parallel_compares_lose_no_index_entry_or_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.sqlite");
        let workspace = dir.path().join("project");
        let shared = Arc::new(Store::open(&path).unwrap());
        const EACH: usize = 10;
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let store = if worker % 2 == 0 {
                    shared.clone()
                } else {
                    Arc::new(Store::open(&path).unwrap())
                };
                let (workspace, barrier) = (workspace.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..EACH {
                        let mut record = record(&workspace);
                        save(&store, &mut record).unwrap();
                        let mut record = load(&store, &record.id).unwrap();
                        record.state = "done".into();
                        record.counted = true;
                        record.count_runs = true;
                        save(&store, &mut record).unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let ids = index(&shared, &workspace).unwrap();
        assert_eq!(ids.len(), 4 * EACH);
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), 4 * EACH);
        let expected = (0, (4 * EACH) as u64);
        assert_eq!(
            board(&shared, &workspace),
            BTreeMap::from([("alpha".into(), expected), ("beta".into(), expected)])
        );
    }

    /// Two writers holding the same revision of one comparison: the second
    /// save is refused, so its run and win are never counted twice.
    #[test]
    fn a_stale_save_conflicts_instead_of_counting_twice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.sqlite");
        let workspace = dir.path().join("project");
        let (first, second) = (Store::open(&path).unwrap(), Store::open(&path).unwrap());
        let mut created = record(&workspace);
        save(&first, &mut created).unwrap();
        let mut mine = load(&first, &created.id).unwrap();
        let mut theirs = load(&second, &created.id).unwrap();
        for record in [&mut mine, &mut theirs] {
            record.state = "applied".into();
            record.counted = true;
            record.count_runs = true;
            record.winner = Some("beta".into());
        }
        save(&first, &mut mine).unwrap();
        let error = save(&second, &mut theirs).unwrap_err();
        assert!(error.is::<Conflict>(), "{error:#}");
        assert_eq!(
            board(&first, &workspace),
            BTreeMap::from([("alpha".into(), (0, 1)), ("beta".into(), (1, 1))])
        );
        // A later save of the fresh record changes nothing on the board.
        let mut fresh = load(&second, &created.id).unwrap();
        assert_eq!(fresh.revision, 2);
        fresh.notes.push("cleanup retried".into());
        save(&second, &mut fresh).unwrap();
        assert_eq!(board(&second, &workspace)["beta"], (1, 1));
        assert_eq!(index(&second, &workspace).unwrap(), vec![created.id]);
    }
    #[tokio::test]
    async fn snapshot_captures_flagged_files_and_preserves_the_source_index() {
        for mode in ["normal", "split", "sparse"] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source");
            let scratch = root.path().join("scratch");
            fs::create_dir(&source).unwrap();
            fs::create_dir(&scratch).unwrap();
            let cancel = CancellationToken::new();
            git(&source, &["init", "-q"], &cancel).await.unwrap();
            for name in ["assumed.txt", "present.txt", "absent.txt", "staged.txt"] {
                fs::write(source.join(name), "base\n").unwrap();
            }
            git(&source, &["add", "."], &cancel).await.unwrap();
            let mut args = IDENTITY.to_vec();
            args.extend(["commit", "-qm", "Fixture base"]);
            git(&source, &args, &cancel).await.unwrap();
            if mode == "sparse" {
                git(
                    &source,
                    &[
                        "sparse-checkout",
                        "set",
                        "--no-cone",
                        "--",
                        "/*",
                        "!/present.txt",
                        "!/absent.txt",
                    ],
                    &cancel,
                )
                .await
                .unwrap();
            }
            git(
                &source,
                &["update-index", "--assume-unchanged", "assumed.txt"],
                &cancel,
            )
            .await
            .unwrap();
            git(
                &source,
                &[
                    "update-index",
                    "--skip-worktree",
                    "present.txt",
                    "absent.txt",
                ],
                &cancel,
            )
            .await
            .unwrap();
            fs::write(source.join("assumed.txt"), "user hidden edit\n").unwrap();
            fs::write(source.join("present.txt"), "user present edit\n").unwrap();
            if source.join("absent.txt").exists() {
                fs::remove_file(source.join("absent.txt")).unwrap();
            }
            fs::write(source.join("staged.txt"), "staged user edit\n").unwrap();
            git(&source, &["add", "staged.txt"], &cancel).await.unwrap();
            fs::write(source.join("staged.txt"), "unstaged user edit\n").unwrap();
            fs::write(source.join("intent.txt"), "intent user edit\n").unwrap();
            git(&source, &["add", "--intent-to-add", "intent.txt"], &cancel)
                .await
                .unwrap();
            if mode == "split" {
                git(&source, &["update-index", "--split-index"], &cancel)
                    .await
                    .unwrap();
            }
            let index = source.join(".git/index");
            let original = fs::read(&index).unwrap();
            let original_time = fs::metadata(&index).unwrap().modified().unwrap();
            let captured = snapshot_for(
                &scratch,
                &source,
                "test capture",
                "Fixture snapshot",
                &cancel,
            )
            .await
            .unwrap();
            for (name, expected) in [
                ("assumed.txt", "user hidden edit"),
                ("present.txt", "user present edit"),
                ("absent.txt", "base"),
                ("staged.txt", "unstaged user edit"),
                ("intent.txt", "intent user edit"),
            ] {
                assert_eq!(
                    git(
                        &source,
                        &["show", &format!("{}:{name}", captured.commit)],
                        &cancel
                    )
                    .await
                    .unwrap(),
                    expected,
                    "mode={mode}, {name}"
                );
            }
            assert_eq!(
                fs::read(&index).unwrap(),
                original,
                "Source index bytes changed"
            );
            assert_eq!(
                fs::metadata(&index).unwrap().modified().unwrap(),
                original_time
            );
            assert!(!source.join("absent.txt").exists());
        }
    }

    #[tokio::test]
    async fn snapshot_preserves_racy_index_timestamp_and_captures_same_stat_edit() {
        use std::time::{Duration, SystemTime};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let scratch = root.path().join("scratch");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&scratch).unwrap();
        let cancel = CancellationToken::new();
        git(&source, &["init", "-q"], &cancel).await.unwrap();
        git(&source, &["config", "core.trustctime", "false"], &cancel)
            .await
            .unwrap();
        git(&source, &["config", "core.checkstat", "minimal"], &cancel)
            .await
            .unwrap();
        let timestamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let file = source.join("data.txt");
        fs::write(&file, b"before\n").unwrap();
        fs::File::open(&file)
            .unwrap()
            .set_modified(timestamp)
            .unwrap();
        fs::write(source.join(".gitignore"), b"forced.txt\n").unwrap();
        git(&source, &["add", "."], &cancel).await.unwrap();
        let mut commit_args = IDENTITY.to_vec();
        commit_args.extend(["commit", "-qm", "Fixture base"]);
        git(&source, &commit_args, &cancel).await.unwrap();
        fs::write(source.join("forced.txt"), b"force-added ignored bytes\n").unwrap();
        git(&source, &["add", "-f", "forced.txt"], &cancel)
            .await
            .unwrap();
        // Snapshot flag normalization rewrites the copied index; it must not
        // make the other same-stat entry look safely older than that index.
        git(
            &source,
            &["update-index", "--assume-unchanged", "forced.txt"],
            &cancel,
        )
        .await
        .unwrap();
        let index = source.join(".git/index");
        fs::File::open(&index)
            .unwrap()
            .set_modified(timestamp)
            .unwrap();
        let index_bytes = fs::read(&index).unwrap();
        let head = git(&source, &["rev-parse", "HEAD"], &cancel).await.unwrap();
        // Same size and cached mtime: Git must use the index's original racy
        // timestamp to decide to hash bytes instead of trusting its stat cache.
        fs::write(&file, b"after!\n").unwrap();
        fs::File::open(&file)
            .unwrap()
            .set_modified(timestamp)
            .unwrap();
        let captured = snapshot_for(
            &scratch,
            &source,
            "test capture",
            "Fixture snapshot",
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(
            git(
                &source,
                &["show", &format!("{}:data.txt", captured.commit)],
                &cancel
            )
            .await
            .unwrap(),
            "after!"
        );
        assert_eq!(
            git(
                &source,
                &["show", &format!("{}:forced.txt", captured.commit)],
                &cancel
            )
            .await
            .unwrap(),
            "force-added ignored bytes"
        );
        assert_eq!(
            fs::read(&index).unwrap(),
            index_bytes,
            "Source index must remain byte-identical"
        );
        assert_eq!(fs::metadata(&index).unwrap().modified().unwrap(), timestamp);
        assert_eq!(
            git(&source, &["rev-parse", "HEAD"], &cancel).await.unwrap(),
            head
        );
        assert_eq!(fs::read(&file).unwrap(), b"after!\n");
    }
}

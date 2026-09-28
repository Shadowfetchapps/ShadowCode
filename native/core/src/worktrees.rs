//! Managed Git worktrees. The source checkout is never reset or stashed.
pub mod changes;
#[cfg(target_os = "linux")]
mod cleanup;
mod locks;
pub mod repair;
use crate::{
    paths::{self, AppPaths},
    process::{self, ProcessSpec},
    workspace::Workspace,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub source: PathBuf,
    pub path: PathBuf,
    pub common_directory: PathBuf,
    pub base_commit: String,
    pub branch: String,
    pub state: String,
    pub created_at: f64,
    pub detail: String,
}
fn roots(paths: &AppPaths) -> Result<(PathBuf, PathBuf)> {
    let root = paths.data.join("managed-worktrees");
    paths::private_directory(&root)?;
    let records = root.join("records");
    paths::private_directory(&records)?;
    let checkouts = root.join("checkouts");
    paths::private_directory(&checkouts)?;
    Ok((records, checkouts))
}
fn save(root: &Path, record: &Record) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(record)?;
    ensure!(
        bytes.len() <= 64_000,
        "Worktree metadata exceeds its recovery-record limit"
    );
    paths::atomic_write(&root.join(format!("{}.json", record.id)), &bytes, true)
}
pub fn list(paths: &AppPaths, source: &Path) -> Result<Vec<Record>> {
    let source = Workspace::open(source)?.path;
    let (root, _) = roots(paths)?;
    let mut records = vec![];
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        ensure!(
            records.len() < 64,
            "Managed worktree inventory exceeds its limit"
        );
        let path = entry.path();
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .context("Invalid managed worktree record filename")?;
        let record = read_record_identity(paths, id)?;
        records.push(record);
    }
    records.retain(|r| r.source == source);
    records.sort_by(|a, b| b.created_at.total_cmp(&a.created_at));
    Ok(records)
}
async fn git(source: &Path, args: &[&str], cancel: CancellationToken) -> Result<String> {
    let mut flags = vec![
        "--no-pager",
        "--literal-pathspecs",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "color.ui=false",
    ];
    // Worktree diffs and change copies never run the repository's own
    // filter drivers (`crate::git_guard`), like worktree preparation.
    let guard = crate::git_guard::args_async(source, args).await?;
    flags.extend(guard.iter().map(String::as_str));
    let hardened = crate::git_guard::harden(args);
    flags.extend(hardened.iter().map(String::as_str));
    let mut spec = ProcessSpec::command("git", &flags, source.into());
    spec.timeout = Duration::from_secs(120);
    spec.output_limit = 64_000;
    let result = process::run(spec, cancel, None).await?;
    ensure!(
        result.ok,
        "Git worktree operation failed: {}{}",
        result.stderr,
        result.stdout
    );
    ensure!(!result.truncated, "Git worktree output exceeded its limit");
    Ok(result.stdout.trim_end_matches('\n').into())
}
/// Lock namespace for task metadata operations. This conveys no destructive
/// authority and never takes the core mutation guard; task callers must still
/// use the normal inspected/journaled worktree APIs for all filesystem changes.
pub(crate) async fn task_common_directory(
    source: &Path,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    let source = Workspace::open(source)?.path;
    let common = git(
        &source,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cancel.clone(),
    )
    .await?;
    Ok(Path::new(&common).canonicalize()?)
}

/// Verify a task's persisted checkout binding without requiring its source or
/// checkout to exist. Archived records support idempotent completed cleanup;
/// malformed/foreign active records never fall back to an older archive.
pub(crate) fn task_record(paths: &AppPaths, id: &str) -> Result<Record> {
    ensure!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "Use the full managed worktree ID"
    );
    let (records, _) = roots(paths)?;
    let archived = match fs::symlink_metadata(records.join(format!("{id}.json"))) {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    read_record_identity_at(paths, id, archived)
}

pub async fn create(
    paths: &AppPaths,
    source: &Path,
    reference: &str,
    cancel: CancellationToken,
) -> Result<Record> {
    let ownership = locks::mutation(source, &cancel).await?;
    create_locked(paths, source, reference, true, &ownership, cancel).await
}
async fn create_locked(
    paths: &AppPaths,
    source: &Path,
    reference: &str,
    ready_after_checkout: bool,
    ownership: &locks::Mutation,
    cancel: CancellationToken,
) -> Result<Record> {
    let source = Workspace::open(source)?.path;
    ensure!(
        !reference.is_empty() && reference.len() <= 256 && !reference.chars().any(char::is_control),
        "Choose a local commit or branch reference"
    );
    let top = git(&source, &["rev-parse", "--show-toplevel"], cancel.clone()).await?;
    ensure!(
        Path::new(&top).canonicalize()? == source,
        "Open the repository root before creating an isolated worktree"
    );
    let commit = git(
        &source,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
        cancel.clone(),
    )
    .await?;
    ensure!(
        matches!(commit.len(), 40 | 64) && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "Git did not return one commit ID"
    );
    let common = PathBuf::from(
        git(
            &source,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            cancel.clone(),
        )
        .await?,
    )
    .canonicalize()?;
    let (records, checkouts) = roots(paths)?;
    ownership.validate(&source, &common)?;
    let id = crate::id();
    let path = checkouts.join(&id);
    let branch = format!("shadowcode/{id}");
    let mut record=Record{id,source:source.clone(),path:path.clone(),common_directory:common,base_commit:commit.clone(),branch:branch.clone(),state:"creating".into(),created_at:crate::now(),detail:"Checkout starts from the selected commit; source uncommitted changes remain in the source checkout.".into()};
    locks::reserve_record(paths, &records, &record, ownership, &cancel).await?;
    // Write the recovery record before Git can create metadata or files. Never
    // recursively remove a partial checkout after failure or cancellation.
    let result = git(
        &source,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            "--",
            path.to_str().context("Worktree path must be UTF-8")?,
            &commit,
        ],
        cancel,
    )
    .await;
    match result {
        Ok(_) => {
            // A copy is not ready until its patches, files and verification finish.
            if ready_after_checkout {
                record.state = "ready".into();
                save(&records, &record)?;
            }
            Ok(record)
        }
        Err(error) => {
            record.state = "needs_attention".into();
            record.detail = format!("{error:#}").chars().take(2000).collect();
            save(&records, &record)?;
            Err(error.context(format!(
                "Recovery record {} preserves any partial worktree at {}",
                record.id,
                path.display()
            )))
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Inspection {
    pub record: Record,
    pub head: String,
    pub current_branch: String,
    pub status: String,
    pub can_remove: bool,
    pub locked: bool,
    pub reason: String,
    pub hash: String,
}
fn read_record_identity(paths: &AppPaths, id: &str) -> Result<Record> {
    read_record_identity_at(paths, id, false)
}
fn read_record_identity_at(paths: &AppPaths, id: &str, archived: bool) -> Result<Record> {
    ensure!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "Use the full managed worktree ID"
    );
    let (records, checkouts) = roots(paths)?;
    let path = if archived {
        records.join("archive")
    } else {
        records
    }
    .join(format!("{id}.json"));
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .context("Managed worktree record not found")?;
    ensure!(
        file.metadata()?.is_file(),
        "Worktree record must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(64_001).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 64_000, "Worktree record exceeds 64 KB");
    let record: Record = serde_json::from_slice(&bytes)?;
    ensure!(
        record.id == id
            && record.path == checkouts.join(id)
            && record.branch == format!("shadowcode/{id}"),
        "Managed worktree record identity changed"
    );
    Ok(record)
}
fn read_record(paths: &AppPaths, source: &Path, id: &str) -> Result<Record> {
    let record = read_record_identity(paths, id)?;
    ensure!(
        record.source == Workspace::open(source)?.path,
        "Worktree belongs to another source project"
    );
    Ok(record)
}
/// Validate the live checkout before staging any task or Compare result. This
/// intentionally avoids a full status/ignored-file scan of the project.
pub(crate) async fn validate_task_checkout(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    checkout: &Path,
    base_commit: &str,
    branch: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    let record = read_record(paths, source, id)?;
    ensure!(
        record.path == checkout && record.base_commit == base_commit && record.branch == branch,
        "Managed task checkout binding changed; preserve it for review"
    );
    let meta = fs::symlink_metadata(checkout)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink() && checkout.canonicalize()? == checkout,
        "Managed checkout path changed"
    );
    let pointer = fs::symlink_metadata(checkout.join(".git"))?;
    ensure!(
        pointer.is_file() && !pointer.file_type().is_symlink(),
        "Managed checkout Git pointer changed"
    );
    let root = git(checkout, &["rev-parse", "--show-toplevel"], cancel.clone()).await?;
    ensure!(
        Path::new(&root).canonicalize()? == record.path,
        "Managed checkout repository root changed"
    );
    for path in [&record.source, &record.path] {
        let common = git(
            path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            cancel.clone(),
        )
        .await?;
        ensure!(
            Path::new(&common).canonicalize()? == record.common_directory,
            "Managed checkout repository identity changed"
        );
    }
    let admin = git(
        checkout,
        &["rev-parse", "--absolute-git-dir"],
        cancel.clone(),
    )
    .await?;
    ensure!(
        Path::new(&admin).canonicalize()?
            == record
                .common_directory
                .join("worktrees")
                .join(id)
                .canonicalize()?,
        "Managed checkout administrative identity changed"
    );
    let current = git(
        checkout,
        &["symbolic-ref", "--quiet", "HEAD"],
        cancel.clone(),
    )
    .await?;
    let expected_branch = format!("refs/heads/{}", record.branch);
    ensure!(
        current == expected_branch,
        "Checkout is no longer on its managed branch; preserve for review"
    );
    let registered = git(
        source,
        &["worktree", "list", "--porcelain", "-z"],
        cancel.clone(),
    )
    .await?;
    let expected = format!(
        "worktree {}",
        checkout.to_str().context("Worktree path must be UTF-8")?
    );
    let registration = registered
        .split("\0\0")
        .find(|block| block.split('\0').any(|field| field == expected))
        .context("Managed checkout is no longer registered with its source")?;
    ensure!(
        registration
            .split('\0')
            .any(|field| field == format!("branch {expected_branch}")),
        "Managed checkout registration branch changed"
    );
    ensure!(
        !registration
            .split('\0')
            .any(|field| field == "locked" || field.starts_with("locked ")),
        "Git worktree is locked; preserve it for review"
    );
    Ok(())
}

pub async fn inspect(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    cancel: CancellationToken,
) -> Result<Inspection> {
    use sha2::{Digest, Sha256};
    let record = read_record(paths, source, id)?;
    let meta = fs::symlink_metadata(&record.path)
        .context("Managed checkout is missing; inspect its recovery record and Git registration")?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "Managed checkout must be a real directory"
    );
    ensure!(
        record.path.canonicalize()? == record.path,
        "Managed checkout path changed"
    );
    let actual = git(
        &record.path,
        &["rev-parse", "--show-toplevel"],
        cancel.clone(),
    )
    .await?;
    ensure!(
        Path::new(&actual).canonicalize()? == record.path,
        "Managed checkout repository root changed"
    );
    for path in [&record.source, &record.path] {
        let common = git(
            path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            cancel.clone(),
        )
        .await?;
        ensure!(
            Path::new(&common).canonicalize()? == record.common_directory,
            "Managed checkout repository identity changed"
        );
    }
    let registered = git(
        &record.source,
        &["worktree", "list", "--porcelain", "-z"],
        cancel.clone(),
    )
    .await?;
    let expected = format!(
        "worktree {}",
        record
            .path
            .to_str()
            .context("Worktree path must be UTF-8")?
    );
    let registration = registered
        .split("\0\0")
        .find(|block| block.split('\0').any(|field| field == expected))
        .context("Managed checkout is no longer registered with its source repository")?;
    let locked = registration
        .split('\0')
        .any(|field| field == "locked" || field.starts_with("locked "));
    let head = git(
        &record.path,
        &["rev-parse", "--verify", "HEAD"],
        cancel.clone(),
    )
    .await?;
    let current_branch = git(&record.path, &["branch", "--show-current"], cancel.clone()).await?;
    let status = git(
        &record.path,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored=matching",
        ],
        cancel,
    )
    .await?;
    let reason = if locked {
        "Git worktree is locked; unlock it deliberately before removal"
    } else if !status.is_empty() {
        "Checkout contains local edits, untracked or ignored files; preserve them before removal"
    } else if current_branch.is_empty() {
        "Detached HEAD may contain unreferenced commits; attach a branch before removal"
    } else {
        "Clean checkout; its branch and commits will be preserved"
    }
    .to_string();
    let can_remove = !locked && status.is_empty() && !current_branch.is_empty();
    let hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(
            &serde_json::json!({"record":record,"head":head,"branch":current_branch,"status":status,"locked":locked})
        )?)
    );
    Ok(Inspection {
        record,
        head,
        current_branch,
        status,
        can_remove,
        locked,
        reason,
        hash,
    })
}
pub async fn remove(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    expected_hash: &str,
    cancel: CancellationToken,
) -> Result<Record> {
    let _ownership = locks::mutation(source, &cancel).await?;
    let inspection = inspect(paths, source, id, cancel.clone()).await?;
    ensure!(
        inspection.hash == expected_hash,
        "Worktree changed; inspect it again before removal"
    );
    ensure!(inspection.can_remove, "{}", inspection.reason);
    let (records, _) = roots(paths)?;
    let mut record = inspection.record;
    record.state = "removing".into();
    record.detail = format!(
        "Removing clean checkout; branch {} is preserved",
        inspection.current_branch
    );
    save(&records, &record)?;
    // Git performs its own final cleanliness/registration checks. Never use
    // --force, branch deletion, or recursive filesystem deletion here.
    let result = git(
        &record.source,
        &[
            "worktree",
            "remove",
            "--",
            record
                .path
                .to_str()
                .context("Worktree path must be UTF-8")?,
        ],
        cancel,
    )
    .await;
    match result {
        Ok(_) => {
            record.state = "removed".into();
            record.detail = format!(
                "Checkout removed; branch {} and commit {} retained",
                inspection.current_branch, inspection.head
            );
            save(&records, &record)?;
            let archive = records.join("archive");
            paths::private_directory(&archive)?;
            fs::rename(
                records.join(format!("{}.json", record.id)),
                archive.join(format!("{}.json", record.id)),
            )?;
            fs::File::open(&records)?.sync_all()?;
            fs::File::open(&archive)?.sync_all()?;
            Ok(record)
        }
        Err(error) => {
            record.state = "needs_attention".into();
            record.detail = format!("{error:#}").chars().take(2000).collect();
            save(&records, &record)?;
            Err(error)
        }
    }
}

/// Remove a disposable ShadowCode-owned checkout and delete its managed
/// `shadowcode/<id>` branch. Compare uses this for lanes whose result the
/// user kept elsewhere or explicitly discarded, and worktree tasks once their
/// result was applied or discarded. Unlike `remove`, build
/// outputs and other ignored files are included in bounded ownership evidence.
/// On Linux disposal moves the owned tree to private quarantine before Git
/// deletion. Absent unjournaled checkouts retain their Git recovery material;
/// large or changed trees remain pending with an explicit review reason.
/// The source checkout, its index and every other branch are never touched.
/// Returns a note when something was left behind or already missing.
pub async fn dispose(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    cancel: CancellationToken,
) -> Result<Option<String>> {
    dispose_checkout(paths, source, id, true, cancel).await
}

/// `dispose`, but the managed `shadowcode/<id>` branch (holding a result the
/// user chose to keep as a branch) stays.
pub async fn release(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    cancel: CancellationToken,
) -> Result<Option<String>> {
    dispose_checkout(paths, source, id, false, cancel).await
}

async fn dispose_checkout(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    delete_branch: bool,
    cancel: CancellationToken,
) -> Result<Option<String>> {
    #[cfg(target_os = "linux")]
    {
        cleanup::dispose(paths, source, id, delete_branch, cancel).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        dispose_checkout_legacy(paths, source, id, delete_branch, cancel).await
    }
}

#[cfg(not(target_os = "linux"))]
async fn dispose_checkout_legacy(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    delete_branch: bool,
    cancel: CancellationToken,
) -> Result<Option<String>> {
    let _ownership = locks::mutation(source, &cancel).await?;
    let mut record = read_record(paths, source, id)?;
    let (records, _) = roots(paths)?;
    let location = record
        .path
        .to_str()
        .context("Worktree path must be UTF-8")?
        .to_owned();
    let missing = matches!(
        fs::symlink_metadata(&record.path),
        Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
    );
    let mut note = None;
    if missing {
        let common = PathBuf::from(
            git(
                &record.source,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                cancel.clone(),
            )
            .await?,
        )
        .canonicalize()?;
        ensure!(
            common == record.common_directory,
            "Source repository identity changed"
        );
        let registered = git(
            &record.source,
            &["worktree", "list", "--porcelain", "-z"],
            cancel.clone(),
        )
        .await?;
        let expected = format!("worktree {location}");
        if let Some(block) = registered
            .split("\0\0")
            .find(|block| block.split('\0').any(|field| field == expected))
        {
            ensure!(
                !block
                    .split('\0')
                    .any(|field| field == "locked" || field.starts_with("locked ")),
                "Worktree is locked; it may be on an unavailable device"
            );
            git(
                &record.source,
                &["worktree", "remove", "--force", "--", &location],
                cancel.clone(),
            )
            .await?;
        }
        note = Some(format!(
            "Checkout {} was already deleted outside ShadowCode; its Git registration and branch were cleaned up",
            record.path.display()
        ));
    } else {
        let inspection = inspect(paths, source, id, cancel.clone()).await?;
        ensure!(
            !inspection.locked,
            "Git worktree is locked; unlock it deliberately before removal"
        );
        ensure!(
            inspection.current_branch == record.branch,
            "Checkout {} is no longer on its managed branch {}; preserve it before removal",
            record.path.display(),
            record.branch
        );
        record.state = "removing".into();
        record.detail = "Removing disposable checkout and its managed branch".into();
        save(&records, &record)?;
        git(
            &record.source,
            &["worktree", "remove", "--force", "--", &location],
            cancel.clone(),
        )
        .await?;
    }
    // Only this record's own branch, whose name `read_record` verified.
    let reference = format!("refs/heads/{}", record.branch);
    if delete_branch
        && git(
            &record.source,
            &["show-ref", "--verify", "--quiet", &reference],
            cancel.clone(),
        )
        .await
        .is_ok()
    {
        if let Err(error) = git(&record.source, &["branch", "-D", &record.branch], cancel).await {
            note = Some(format!("Branch {} was kept: {error:#}", record.branch));
        }
    }
    record.state = "removed".into();
    record.detail = if delete_branch {
        format!(
            "Disposable checkout removed; branch {} deleted",
            record.branch
        )
    } else {
        format!("Checkout removed; branch {} kept", record.branch)
    };
    save(&records, &record)?;
    let archive = records.join("archive");
    paths::private_directory(&archive)?;
    fs::rename(
        records.join(format!("{}.json", record.id)),
        archive.join(format!("{}.json", record.id)),
    )?;
    fs::File::open(&records)?.sync_all()?;
    fs::File::open(&archive)?.sync_all()?;
    Ok(note)
}

/// A rescue creates a new checkout; it never prunes the missing checkout's
/// registration or index, which may still contain recoverable staged changes.
#[derive(Clone, Debug, Serialize)]
pub struct Recovery {
    pub record: Record,
    pub commit: String,
    pub branch: String,
    pub warning: String,
    pub hash: String,
}
pub async fn recovery(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    cancel: CancellationToken,
) -> Result<Recovery> {
    use sha2::{Digest, Sha256};
    let record = read_record(paths, source, id)?;
    match fs::symlink_metadata(&record.path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => anyhow::bail!(
            "Checkout path still exists or cannot be inspected; preserve it before recovery"
        ),
    }
    let common = PathBuf::from(
        git(
            source,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            cancel.clone(),
        )
        .await?,
    )
    .canonicalize()?;
    ensure!(
        common == record.common_directory,
        "Source repository identity changed"
    );
    let registered = git(
        source,
        &["worktree", "list", "--porcelain", "-z"],
        cancel.clone(),
    )
    .await?;
    let expected = format!(
        "worktree {}",
        record
            .path
            .to_str()
            .context("Worktree path must be UTF-8")?
    );
    let block = registered
        .split("\0\0")
        .find(|block| block.split('\0').any(|field| field == expected))
        .context(
            "Missing checkout registration is unavailable; inspect retained branches manually",
        )?;
    ensure!(!block.split('\0').any(|field| field == "locked" || field.starts_with("locked ")), "Worktree is locked; it may be on an unavailable device. Review its location before recovery");
    let commit = block
        .split('\0')
        .find_map(|field| field.strip_prefix("HEAD "))
        .context("Registered checkout has no retained commit")?
        .to_string();
    ensure!(
        matches!(commit.len(), 40 | 64) && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid retained commit"
    );
    let resolved = git(
        source,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
        cancel,
    )
    .await?;
    ensure!(resolved == commit, "Retained commit is unavailable");
    let branch = block
        .split('\0')
        .find_map(|field| field.strip_prefix("branch refs/heads/"))
        .unwrap_or("")
        .to_string();
    let hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(
            &serde_json::json!({"record":record,"registration":block})
        )?)
    );
    Ok(Recovery { record, commit, branch, hash, warning:"Creates a separate checkout from the retained commit. Missing uncommitted files are not reconstructed. The original branch, registration, index and recovery record remain intact for manual recovery.".into() })
}
pub async fn restore(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    expected_hash: &str,
    cancel: CancellationToken,
) -> Result<Record> {
    let review = recovery(paths, source, id, cancel.clone()).await?;
    ensure!(
        review.hash == expected_hash,
        "Recovery state changed; inspect it again before restoring"
    );
    // Commit objects are immutable. Create from the reviewed commit, never a
    // mutable branch name, and leave the original recovery evidence untouched.
    create(paths, source, &review.commit, cancel).await
}

#[derive(Clone, Debug, Serialize)]
pub struct ReturnReview {
    pub record: Record,
    pub source_head: String,
    pub source_branch: String,
    pub worktree_head: String,
    pub worktree_branch: String,
    pub merge_base: String,
    pub diff: String,
    pub hash: String,
}
pub async fn review_return(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    cancel: CancellationToken,
) -> Result<ReturnReview> {
    use sha2::{Digest, Sha256};
    let inspected = inspect(paths, source, id, cancel.clone()).await?;
    let source_top = git(source, &["rev-parse", "--show-toplevel"], cancel.clone()).await?;
    ensure!(
        Path::new(&source_top).canonicalize()? == Workspace::open(source)?.path,
        "Source checkout repository root changed"
    );
    ensure!(
        !inspected.locked,
        "Unlock the worktree deliberately before returning changes"
    );
    ensure!(
        !inspected.current_branch.is_empty(),
        "Attach the worktree to a branch before returning changes"
    );
    for checkout in [source, inspected.record.path.as_path()] {
        let status = git(
            checkout,
            &["status", "--porcelain=v1", "--untracked-files=all"],
            cancel.clone(),
        )
        .await?;
        ensure!(status.is_empty(), "Commit or preserve tracked and untracked changes in both checkouts before returning work");
        for marker in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
        ] {
            let marker_path = git(
                checkout,
                &["rev-parse", "--path-format=absolute", "--git-path", marker],
                cancel.clone(),
            )
            .await?;
            ensure!(
                !Path::new(&marker_path).try_exists()?,
                "Finish the existing Git operation before returning work"
            );
        }
    }
    let source_head = git(source, &["rev-parse", "--verify", "HEAD"], cancel.clone()).await?;
    let source_branch = git(source, &["branch", "--show-current"], cancel.clone()).await?;
    ensure!(
        !source_branch.is_empty(),
        "Attach the source checkout to a branch before returning changes"
    );
    let merge_base = git(
        source,
        &["merge-base", &source_head, &inspected.head],
        cancel.clone(),
    )
    .await?;
    ensure!(
        merge_base != inspected.head,
        "These worktree commits are already present in the source branch"
    );
    // Git's no-overwrite-ignore behavior is not sufficient for every merge
    // strategy. Reject incoming paths overlapping ignored files/directories.
    let incoming = git(
        source,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            &merge_base,
            &inspected.head,
            "--",
        ],
        cancel.clone(),
    )
    .await?;
    let ignored = git(
        source,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
        cancel.clone(),
    )
    .await?;
    for local in ignored.split('\0').filter(|s| !s.is_empty()) {
        let local = local.trim_end_matches('/');
        ensure!(!incoming.split('\0').filter(|s| !s.is_empty()).any(|path| path == local || path.starts_with(&format!("{local}/")) || local.starts_with(&format!("{path}/"))), "Incoming changes overlap ignored source files at {local}; preserve them before returning work");
    }
    let diff = git(
        source,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            &merge_base,
            &inspected.head,
            "--",
        ],
        cancel,
    )
    .await?;
    let mut review = ReturnReview {
        record: inspected.record,
        source_head,
        source_branch,
        worktree_head: inspected.head,
        worktree_branch: inspected.current_branch,
        merge_base,
        diff,
        hash: String::new(),
    };
    review.hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&review)?));
    Ok(review)
}
/// Prepare a merge for the existing source review/commit flow. Never move HEAD,
/// auto-commit, reset, stash, prune, or discard conflicts on the user's behalf.
pub async fn return_changes(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    expected_hash: &str,
    cancel: CancellationToken,
) -> Result<Record> {
    let _ownership = locks::mutation(source, &cancel).await?;
    let review = review_return(paths, source, id, cancel.clone()).await?;
    ensure!(
        review.hash == expected_hash,
        "Branches or review changed; inspect the return again"
    );
    let (records, _) = roots(paths)?;
    let mut record = review.record;
    record.state = "returning".into();
    record.detail = format!("Preparing reviewed merge of {} into {} at {}; inspect source Git status after interruption", review.worktree_head, review.source_branch, review.source_head);
    save(&records, &record)?;
    let merged = git(
        source,
        &[
            "merge",
            "--no-commit",
            "--no-ff",
            "--no-edit",
            "--no-overwrite-ignore",
            "--no-autostash",
            &review.worktree_head,
        ],
        cancel,
    )
    .await;
    match merged {
        Ok(_) => {
            record.state = "merge_pending".into();
            record.detail = "Changes returned to the source index without committing. Review and commit in the source project, or use Git merge --abort to abandon this merge. The worktree and branch remain intact.".into();
        }
        Err(error) => {
            record.state = "needs_attention".into();
            record.detail = format!("Return stopped: {error:#}. Inspect source Git status; resolve any conflicts and commit, or use Git merge --abort. No automatic reset or cleanup was performed.").chars().take(2000).collect();
        }
    }
    save(&records, &record)?;
    Ok(record)
}

//! Whole-project checkpoints around changes ShadowCode does not make itself:
//! native shell commands and subscription CLI turns.
//!
//! In a Git repository the project (tracked files plus untracked files that
//! are not ignored) is captured as a commit built in a temporary index, so the
//! user's index and working tree are never touched, and kept under
//! `refs/shadowcode/checkpoints/<session>/<n>`. Afterwards the project is
//! captured again and every changed file is recorded in the task's file
//! checkpoint with its earlier content, so the existing rewind restores it
//! (and refuses when a file changed again since).
//!
//! Folders without Git are copied, within `checkpoints.max_copy_files` and
//! `max_copy_bytes`; a larger folder is reported as not covered.
//!
//! Not covered: Git-ignored files, files over 4 MB, symlinks, submodules,
//! files using a Git filter (for example LFS), empty directories, and Git
//! state itself (HEAD, branches, the index, stashes).
use super::{record_external, CheckpointConfig};
use crate::{
    store::Store,
    workspace::{hash, Workspace, MAX_FILE_BYTES},
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime},
};

const REF_ROOT: &str = "refs/shadowcode/checkpoints";
/// Changed paths recorded per capture; the rest are reported as skipped.
const MAX_RECORDED: usize = 5_000;
/// Earlier file contents read back from Git per capture.
const MAX_READS: usize = 1_000;
/// Folders a file-copy checkpoint never walks into.
const COPY_SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "target",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
];

/// A capture taken before the command or turn.
pub struct Before {
    kind: Kind,
}
enum Kind {
    Git(Box<GitBase>),
    Copy(BTreeMap<String, Entry>),
    Unavailable(String),
    NotNeeded,
}
struct GitBase {
    dir: PathBuf,
    prefix: String,
    index_dir: tempfile::TempDir,
    tree: String,
    reference: Option<String>,
    large_before: HashSet<String>,
}
struct Entry {
    bytes: Option<Vec<u8>>,
    mode: u32,
    size: u64,
    modified: Option<SystemTime>,
}

/// What a capture recorded.
#[derive(Debug, Default)]
pub struct Outcome {
    pub method: &'static str,
    pub paths: Vec<String>,
    pub skipped: Vec<Value>,
    pub unavailable: Option<String>,
    pub reference: Option<String>,
}
impl Outcome {
    pub fn to_json(&self) -> Value {
        json!({
            "method": self.method,
            "paths": self.paths,
            "skipped": self.skipped,
            "unavailable": self.unavailable,
            "ref": self.reference,
        })
    }
    /// A sentence for the transcript when some changes cannot be rewound.
    pub fn warning(&self, what: &str) -> Option<String> {
        if let Some(reason) = &self.unavailable {
            return Some(format!("Rewind does not cover {what}: {reason}"));
        }
        if self.skipped.is_empty() {
            return None;
        }
        let shown: Vec<String> = self
            .skipped
            .iter()
            .take(5)
            .map(|s| {
                format!(
                    "{} ({})",
                    s["path"].as_str().unwrap_or("?"),
                    s["reason"].as_str().unwrap_or("")
                )
            })
            .collect();
        let more = self.skipped.len().saturating_sub(shown.len());
        Some(format!(
            "Rewind cannot restore {} file(s) changed by {what}: {}{}",
            self.skipped.len(),
            shown.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ))
    }
}

/// Commands that cannot change files, so no checkpoint is needed: a single
/// simple reader with no redirection, pipes, substitution or chaining.
pub fn cannot_write(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty()
        || command.contains([
            '>', '<', '|', ';', '&', '$', '`', '(', ')', '\n', '\r', '\\',
        ])
    {
        return false;
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    match words.as_slice() {
        [first, rest @ ..] => {
            let reader = matches!(
                *first,
                "ls" | "pwd"
                    | "cat"
                    | "head"
                    | "tail"
                    | "wc"
                    | "grep"
                    | "rg"
                    | "which"
                    | "echo"
                    | "stat"
                    | "du"
                    | "df"
                    | "uname"
                    | "date"
                    | "whoami"
                    | "id"
                    | "true"
            ) && !rest.iter().any(|a| a.starts_with("--pre"));
            let git_reader = *first == "git"
                && matches!(
                    rest.first().copied(),
                    Some("status" | "diff" | "log" | "show" | "blame" | "rev-parse" | "ls-files")
                )
                && !rest
                    .iter()
                    .any(|a| a.starts_with("--output") || a.starts_with("--ext-diff"));
            reader || git_reader
        }
        [] => false,
    }
}

fn sanitize(component: &str) -> String {
    let clean: String = component
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(80)
        .collect();
    if clean.is_empty() {
        "session".into()
    } else {
        clean
    }
}

async fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>> {
    // Checkpoints run right after shell commands, which may have changed
    // `.git/config`: never run the repository's own filter drivers.
    let guard = crate::git_guard::args_async(dir, args).await?;
    let mut command = tokio::process::Command::new("git");
    command
        .args([
            "--no-pager",
            "--literal-pathspecs",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.quotepath=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
        ])
        .args(&guard)
        .args(crate::git_guard::harden(args))
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
    ] {
        command.env_remove(name);
    }
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = tokio::time::timeout(Duration::from_secs(300), command.output())
        .await
        .context("Git timed out while taking a checkpoint")??;
    ensure!(
        output.status.success(),
        "Git {} failed: {}",
        args.first().copied().unwrap_or(""),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}
fn line(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes)
        .trim_end_matches('\n')
        .to_owned()
}

/// Capture the project before a command or turn. Never fails: a capture that
/// cannot be taken is reported by [`after`] instead.
pub async fn before(
    workspace: &Path,
    session: &str,
    label: &str,
    config: &CheckpointConfig,
) -> Before {
    let kind = match git_before(workspace, session, label, config.keep).await {
        Ok(Some(base)) => Kind::Git(Box::new(base)),
        Ok(None) => {
            let dir = workspace.to_path_buf();
            let (files, bytes) = (config.max_copy_files, config.max_copy_bytes);
            match tokio::task::spawn_blocking(move || copy_before(&dir, files, bytes)).await {
                Ok(Ok(files)) => Kind::Copy(files),
                Ok(Err(reason)) => Kind::Unavailable(reason),
                Err(error) => Kind::Unavailable(format!("the file copy failed ({error})")),
            }
        }
        Err(error) => Kind::Unavailable(format!("the Git checkpoint failed ({error:#})")),
    };
    Before { kind }
}

/// No capture: the command cannot write, or checkpoints are off.
pub fn not_needed() -> Before {
    Before {
        kind: Kind::NotNeeded,
    }
}

/// Compare with the project now and record every changed file in `task`'s
/// file checkpoint, so rewind restores it.
pub async fn after(
    before: Before,
    store: &Store,
    workspace: &Workspace,
    task: &str,
) -> Result<Outcome> {
    match before.kind {
        Kind::NotNeeded => Ok(Outcome {
            method: "none",
            ..Outcome::default()
        }),
        Kind::Unavailable(reason) => Ok(Outcome {
            method: "none",
            unavailable: Some(reason),
            ..Outcome::default()
        }),
        Kind::Git(base) => git_after(*base, store, workspace, task).await,
        Kind::Copy(files) => {
            let dir = workspace.path.clone();
            let changes = tokio::task::spawn_blocking(move || copy_after(&dir, files)).await??;
            let (paths, skipped) = record(store, workspace, task, changes)?;
            Ok(Outcome {
                method: "copy",
                paths,
                skipped,
                ..Outcome::default()
            })
        }
    }
}

struct Change {
    path: String,
    before: Option<Vec<u8>>,
    before_mode: Option<u32>,
    skip: Option<String>,
}

fn record(
    store: &Store,
    workspace: &Workspace,
    task: &str,
    changes: Vec<Change>,
) -> Result<(Vec<String>, Vec<Value>)> {
    let mut paths = Vec::new();
    let mut skipped = Vec::new();
    for change in changes {
        let mut skip = |reason: &str| {
            skipped.push(json!({"path":change.path,"reason":reason}));
        };
        if let Some(reason) = &change.skip {
            skip(reason);
            continue;
        }
        if paths.len() >= MAX_RECORDED {
            skip("too many changed files in one step");
            continue;
        }
        if workspace.writable(&change.path).is_err() {
            skip("inside Git metadata or reached through a symlink");
            continue;
        }
        let current = match workspace.snapshot(&change.path) {
            Ok(current) => current,
            Err(_) => {
                skip("now larger than 4 MB or not a regular file");
                continue;
            }
        };
        #[cfg(unix)]
        let mode = change.before_mode.or_else(|| {
            // Restoring content keeps the file's current mode; an earlier
            // non-executable file that became executable is reset.
            current
                .mode
                .filter(|m| m & 0o111 != 0 && change.before.is_some())
                .map(|_| 0o644)
        });
        #[cfg(not(unix))]
        let mode = change.before_mode;
        record_external(
            store,
            workspace,
            task,
            &change.path,
            change.before.as_deref(),
            mode,
            current.hash.as_deref(),
        )?;
        paths.push(change.path);
    }
    Ok((paths, skipped))
}

// ---------------------------------------------------------------- Git ----

async fn stage(dir: &Path, index: &Path, scratch: &Path) -> Result<HashSet<String>> {
    // `add` checks submodules and nested repositories by running Git inside
    // them, under their own configuration: freeze their entries first.
    crate::git_guard::freeze_gitlinks(dir, Some(index)).await?;
    git(dir, &["add", "-u", "--", "."], Some(index)).await?;
    let listed = git(
        dir,
        &[
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
            "--",
            ".",
        ],
        Some(index),
    )
    .await?;
    let mut small = Vec::new();
    let mut large = HashSet::new();
    for raw in listed.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let Ok(name) = std::str::from_utf8(raw) else {
            continue;
        };
        match std::fs::symlink_metadata(dir.join(name)) {
            Ok(meta) if meta.is_file() && meta.len() <= MAX_FILE_BYTES as u64 => {
                small.extend_from_slice(raw);
                small.push(0);
            }
            Ok(meta) if meta.is_file() => {
                large.insert(name.to_owned());
            }
            _ => {}
        }
    }
    if !small.is_empty() {
        let list = scratch.join("untracked");
        std::fs::write(&list, &small)?;
        let list = list.to_string_lossy().into_owned();
        git(
            dir,
            &["add", "--pathspec-from-file", &list, "--pathspec-file-nul"],
            Some(index),
        )
        .await?;
    }
    Ok(large)
}

async fn git_before(
    dir: &Path,
    session: &str,
    label: &str,
    keep: usize,
) -> Result<Option<GitBase>> {
    match git(dir, &["rev-parse", "--is-inside-work-tree"], None).await {
        Ok(out) if out.trim_ascii() == b"true" => {}
        _ => return Ok(None),
    }
    let prefix = line(git(dir, &["rev-parse", "--show-prefix"], None).await?);
    let head = git(dir, &["rev-parse", "--verify", "-q", "HEAD^{commit}"], None)
        .await
        .ok()
        .map(line)
        .filter(|h| !h.is_empty());
    let real_index = PathBuf::from(line(
        git(
            dir,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
            None,
        )
        .await?,
    ));
    let index_dir = tempfile::Builder::new()
        .prefix("shadowcode-checkpoint-")
        .tempdir()?;
    let index = index_dir.path().join("index");
    // A copy of the real index keeps its stat cache (fast) and force-added
    // files; a fresh index from HEAD is the fallback (e.g. a split index).
    let copied = real_index.is_file() && std::fs::copy(&real_index, &index).is_ok();
    let first = if copied {
        stage(dir, &index, index_dir.path()).await.ok()
    } else {
        None
    };
    let large_before = match first {
        Some(large) => large,
        None => {
            let _ = std::fs::remove_file(&index);
            if let Some(head) = &head {
                git(dir, &["read-tree", head], Some(&index)).await?;
            }
            stage(dir, &index, index_dir.path()).await?
        }
    };
    let tree = line(git(dir, &["write-tree"], Some(&index)).await?);
    let message = format!("ShadowCode checkpoint before {label}");
    let mut args = vec![
        "-c",
        "user.name=ShadowCode",
        "-c",
        "user.email=shadowcode@localhost",
        "-c",
        "commit.gpgsign=false",
        "commit-tree",
        &tree,
        "-m",
        &message,
    ];
    if let Some(head) = &head {
        args.extend(["-p", head]);
    }
    let commit = line(git(dir, &args, None).await?);
    let reference = create_ref(dir, &sanitize(session), &commit).await.ok();
    let _ = prune(dir, keep).await;
    Ok(Some(GitBase {
        dir: dir.to_path_buf(),
        prefix,
        index_dir,
        tree,
        reference,
        large_before,
    }))
}

async fn create_ref(dir: &Path, session: &str, commit: &str) -> Result<String> {
    let base = format!("{REF_ROOT}/{session}/");
    let existing = git(dir, &["for-each-ref", "--format=%(refname)", &base], None).await?;
    let first = String::from_utf8_lossy(&existing)
        .lines()
        .filter_map(|r| r.strip_prefix(&base)?.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    let mut last = None;
    for next in first..first + 5 {
        let name = format!("{base}{next}");
        // An empty old value: create only, never overwrite a parallel capture.
        match git(dir, &["update-ref", &name, commit, ""], None).await {
            Ok(_) => return Ok(name),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("Could not create a checkpoint ref")))
}

/// Keep the newest `keep` checkpoint refs of this repository.
async fn prune(dir: &Path, keep: usize) -> Result<usize> {
    let listed = git(
        dir,
        &[
            "for-each-ref",
            "--sort=-creatordate",
            "--format=%(refname)",
            REF_ROOT,
        ],
        None,
    )
    .await?;
    let text = String::from_utf8_lossy(&listed).into_owned();
    let mut removed = 0;
    for name in text.lines().skip(keep).take(100) {
        if git(dir, &["update-ref", "-d", name], None).await.is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

async fn git_after(
    base: GitBase,
    store: &Store,
    workspace: &Workspace,
    task: &str,
) -> Result<Outcome> {
    let dir = &base.dir;
    let index = base.index_dir.path().join("index");
    stage(dir, &index, base.index_dir.path()).await?;
    let tree = line(git(dir, &["write-tree"], Some(&index)).await?);
    let mut outcome = Outcome {
        method: "git",
        reference: base.reference.clone(),
        ..Outcome::default()
    };
    let raw = if tree == base.tree {
        Vec::new()
    } else {
        git(
            dir,
            &["diff-tree", "-r", "-z", "--no-renames", &base.tree, &tree],
            None,
        )
        .await?
    };
    // Raw -z output: ":old_mode new_mode old_sha new_sha status\0path\0".
    let fields: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    let mut entries = Vec::new();
    for pair in fields.chunks(2) {
        let [meta, path] = pair else { continue };
        let meta = String::from_utf8_lossy(meta);
        let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let (Some(old_mode), Some(new_mode), Some(old_sha)) =
            (parts.first(), parts.get(1), parts.get(2))
        else {
            continue;
        };
        let Ok(full) = std::str::from_utf8(path) else {
            continue;
        };
        let Some(relative) = full.strip_prefix(base.prefix.as_str()) else {
            continue;
        };
        entries.push((
            old_mode.to_string(),
            new_mode.to_string(),
            old_sha.to_string(),
            full.to_owned(),
            relative.to_owned(),
        ));
    }
    let filtered = filtered_paths(dir, &entries).await;
    let mut reads = 0;
    let mut changes = Vec::new();
    for (old_mode, new_mode, old_sha, full, relative) in entries {
        let mut change = Change {
            path: relative.clone(),
            before: None,
            before_mode: None,
            skip: None,
        };
        let special = |m: &str| m == "120000" || m == "160000";
        if special(&old_mode) || special(&new_mode) {
            change.skip = Some("symlink or submodule".into());
        } else if old_mode == "000000" && base.large_before.contains(&relative) {
            change.skip = Some("was larger than 4 MB before".into());
        } else if filtered.contains(&full) {
            change.skip = Some("uses a Git filter such as LFS".into());
        } else if old_mode != "000000" {
            if reads >= MAX_READS {
                change.skip = Some("too many changed files in one step".into());
            } else {
                reads += 1;
                let object = format!("{}:{full}", base.tree);
                match git(dir, &["cat-file", "--filters", &object], None).await {
                    Ok(bytes) if bytes.len() <= MAX_FILE_BYTES => {
                        change.before = Some(bytes);
                        change.before_mode = (old_mode == "100755").then_some(0o755);
                    }
                    Ok(_) => change.skip = Some("was larger than 4 MB before".into()),
                    Err(_) => {
                        // Fall back to the stored blob when filters fail.
                        match git(dir, &["cat-file", "blob", &old_sha], None).await {
                            Ok(bytes) if bytes.len() <= MAX_FILE_BYTES => {
                                change.before = Some(bytes);
                                change.before_mode = (old_mode == "100755").then_some(0o755);
                            }
                            _ => change.skip = Some("earlier content unreadable".into()),
                        }
                    }
                }
            }
        }
        changes.push(change);
    }
    let (paths, skipped) = record(store, workspace, task, changes)?;
    outcome.paths = paths;
    outcome.skipped = skipped;
    if outcome.paths.is_empty() && outcome.skipped.is_empty() {
        // Nothing changed: the checkpoint ref is not worth keeping.
        if let Some(name) = &base.reference {
            let _ = git(dir, &["update-ref", "-d", name], None).await;
        }
        outcome.reference = None;
    }
    Ok(outcome)
}

/// Paths with a `filter` attribute (LFS and similar), whose earlier content
/// would need an external program to reproduce.
async fn filtered_paths(
    dir: &Path,
    entries: &[(String, String, String, String, String)],
) -> HashSet<String> {
    let mut filtered = HashSet::new();
    let top = match git(dir, &["rev-parse", "--show-toplevel"], None).await {
        Ok(out) => PathBuf::from(line(out)),
        Err(_) => return filtered,
    };
    for chunk in entries.chunks(200) {
        let mut args = vec!["check-attr", "-z", "filter", "--"];
        args.extend(chunk.iter().map(|e| e.3.as_str()));
        let Ok(out) = git(&top, &args, None).await else {
            continue;
        };
        // "path\0attribute\0value\0" triples.
        let fields: Vec<&[u8]> = out.split(|b| *b == 0).collect();
        for triple in fields.chunks(3) {
            if let [path, _, value] = triple {
                if !matches!(*value, b"unspecified" | b"unset" | b"") {
                    filtered.insert(String::from_utf8_lossy(path).into_owned());
                }
            }
        }
    }
    filtered
}

// ------------------------------------------------------------ file copy ----

fn walk(dir: &Path) -> impl Iterator<Item = (String, std::fs::Metadata)> + '_ {
    ignore::WalkBuilder::new(dir)
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| {
            !(entry.file_type().is_some_and(|t| t.is_dir())
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| COPY_SKIP_DIRS.contains(&name)))
        })
        .build()
        .filter_map(Result::ok)
        .filter_map(move |entry| {
            let meta = std::fs::symlink_metadata(entry.path()).ok()?;
            if !meta.is_file() {
                return None;
            }
            let relative = entry.path().strip_prefix(dir).ok()?.to_str()?.to_owned();
            Some((relative, meta))
        })
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}
#[cfg(not(unix))]
fn mode_of(_: &std::fs::Metadata) -> u32 {
    0o644
}

fn copy_before(
    dir: &Path,
    max_files: usize,
    max_bytes: u64,
) -> std::result::Result<BTreeMap<String, Entry>, String> {
    let too_big = || {
        format!("this folder is not a Git repository and has more than {max_files} files or {} MB; put it under Git for full undo", max_bytes / (1024 * 1024))
    };
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    for (relative, meta) in walk(dir) {
        if files.len() >= max_files {
            return Err(too_big());
        }
        let size = meta.len();
        let bytes = if size <= MAX_FILE_BYTES as u64 {
            total += size;
            if total > max_bytes {
                return Err(too_big());
            }
            match std::fs::read(dir.join(&relative)) {
                Ok(bytes) => Some(bytes),
                Err(_) => continue,
            }
        } else {
            None
        };
        files.insert(
            relative,
            Entry {
                bytes,
                mode: mode_of(&meta),
                size,
                modified: meta.modified().ok(),
            },
        );
    }
    Ok(files)
}

fn copy_after(dir: &Path, mut before: BTreeMap<String, Entry>) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    for (relative, meta) in walk(dir) {
        match before.remove(&relative) {
            None => changes.push(Change {
                path: relative,
                before: None,
                before_mode: None,
                skip: None,
            }),
            Some(old) => {
                let same_stat = old.size == meta.len() && old.modified == meta.modified().ok();
                match old.bytes {
                    Some(bytes) => {
                        let changed = !same_stat
                            && std::fs::read(dir.join(&relative))
                                .map(|now| hash(&now) != hash(&bytes))
                                .unwrap_or(true);
                        if changed || old.mode != mode_of(&meta) {
                            changes.push(Change {
                                path: relative,
                                before: Some(bytes),
                                before_mode: Some(old.mode),
                                skip: None,
                            });
                        }
                    }
                    None if !same_stat => changes.push(Change {
                        path: relative,
                        before: None,
                        before_mode: None,
                        skip: Some("larger than 4 MB".into()),
                    }),
                    None => {}
                }
            }
        }
    }
    for (relative, old) in before {
        changes.push(Change {
            path: relative,
            skip: old.bytes.is_none().then(|| "larger than 4 MB".into()),
            before: old.bytes,
            before_mode: Some(old.mode),
        });
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A shell command can write `.git/config`; the checkpoint that follows
    /// runs Git outside the sandbox and must not run a filter it defines.
    #[tokio::test]
    async fn checkpoints_never_run_repository_filters() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), b"one\n").unwrap();
        run(&repo, &["add", "a.txt"]);
        run(&repo, &["commit", "-qm", "base"]);
        let marker = root.path().join("filter-ran");
        let command = format!("touch '{}'; cat", marker.display());
        run(&repo, &["config", "filter.probe.clean", &command]);
        run(&repo, &["config", "filter.probe.smudge", &command]);
        std::fs::write(repo.join(".gitattributes"), b"* filter=probe\n").unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
        std::fs::write(repo.join("new.txt"), b"new\n").unwrap();
        let before = before(&repo, "s1", "test", &CheckpointConfig::default()).await;
        assert!(
            matches!(before.kind, Kind::Git(_)),
            "the checkpoint is still taken"
        );
        assert!(!marker.exists(), "the checkpoint ran a repository filter");
    }

    /// A nested repository's own configuration is not inspected: the
    /// checkpoint must not run Git inside it at all.
    #[tokio::test]
    async fn checkpoints_never_run_nested_repository_filters() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let inner = repo.join("vendor");
        std::fs::create_dir_all(&inner).unwrap();
        run(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), b"one\n").unwrap();
        run(&repo, &["add", "a.txt"]);
        run(&inner, &["init", "-q"]);
        std::fs::write(inner.join("f.txt"), b"one\n").unwrap();
        run(&inner, &["add", "f.txt"]);
        run(&inner, &["commit", "-qm", "inner"]);
        let head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&inner)
            .output()
            .unwrap();
        let entry = format!(
            "160000,{},vendor",
            String::from_utf8_lossy(&head.stdout).trim()
        );
        run(&repo, &["update-index", "--add", "--cacheinfo", &entry]);
        run(&repo, &["commit", "-qm", "base"]);
        let marker = root.path().join("nested-filter-ran");
        let command = format!("touch '{}'; cat", marker.display());
        run(&inner, &["config", "filter.nested.clean", &command]);
        std::fs::write(inner.join(".gitattributes"), b"* filter=nested\n").unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(inner.join("f.txt"), b"two\n").unwrap();
        std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
        let before = before(&repo, "s1", "test", &CheckpointConfig::default()).await;
        assert!(
            matches!(before.kind, Kind::Git(_)),
            "the checkpoint is still taken"
        );
        assert!(
            !marker.exists(),
            "the checkpoint ran a nested repository's filter"
        );
    }

    #[test]
    fn simple_readers_skip_the_checkpoint() {
        for command in [
            "ls -la",
            "cat src/main.rs",
            "git status",
            "git diff HEAD",
            "rg foo",
        ] {
            assert!(cannot_write(command), "{command}");
        }
        for command in [
            "echo hi > f",
            "cat a | tee b",
            "ls; rm x",
            "rm -rf build",
            "git diff --output=patch",
            "git checkout .",
            "rg --pre ./x foo",
            "cargo build",
            "FOO=1 cat x",
            "$(touch x)",
        ] {
            assert!(!cannot_write(command), "{command}");
        }
    }
}

//! Per-task review: what one task changed, file by file and hunk by hunk,
//! undoing single hunks or whole files, and rewinds that can be undone.
//!
//! A file's baseline is the checkpoint taken before the task's first write to
//! it (`checkpoint::record`). Vendor CLIs edit files with their own tools, so
//! a path they reported without a checkpoint is compared with the last commit
//! instead (`source: "git"`); the review says so.
//!
//! Undo writes the file only when it still has the content the diff was made
//! from, and moves the checkpoint's "after" to the new content so Rewind keeps
//! working. A rewind first records the files as they are under an undo id
//! (`rewind:<id>`, the same checkpoint table), so Undo can put them back.
use crate::{
    checkpoint,
    store::{keys, Store},
    textdiff,
    workspace::{hash, Snapshot, Workspace},
};
use anyhow::{bail, ensure, Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

/// Diffs of files larger than this are not shown (undo works file-wide).
const MAX_DIFF_BYTES: usize = 2_000_000;

/// Where a file's "before" comes from.
#[derive(Clone, Debug, PartialEq)]
struct Baseline {
    source: &'static str,
    bytes: Option<Vec<u8>>,
    mode: Option<u32>,
}

fn checkpoint_row(
    store: &Store,
    ws: &Workspace,
    task: &str,
    path: &str,
) -> Result<Option<Baseline>> {
    let db = store.lock()?;
    let mut statement = db.prepare(
        "SELECT before_bytes,before_mode FROM file_changes WHERE task_id=? AND workspace=? AND path=?",
    )?;
    let mut rows = statement.query(params![task, ws.path.to_string_lossy(), path])?;
    Ok(match rows.next()? {
        Some(row) => Some(Baseline {
            source: "checkpoint",
            bytes: row.get(0)?,
            mode: row.get(1)?,
        }),
        None => None,
    })
}

fn checkpoint_paths(store: &Store, ws: &Workspace, task: &str) -> Result<Vec<String>> {
    Ok(store
        .query(
            "SELECT path FROM file_changes WHERE task_id=? AND workspace=? ORDER BY id",
            params![task, ws.path.to_string_lossy()],
        )?
        .iter()
        .filter_map(|row| row["path"].as_str().map(str::to_owned))
        .collect())
}

/// Every path the task changed: its checkpoint rows first, then paths a
/// vendor CLI reported, relative to the project.
pub fn task_paths(store: &Store, ws: &Workspace, task: &str) -> Result<Vec<String>> {
    let mut paths = checkpoint_paths(store, ws, task)?;
    for reported in store.changed_files(&[task.to_owned()])? {
        let Ok(relative) = ws.relative(&reported) else {
            continue;
        };
        let relative = relative.to_string_lossy().into_owned();
        if relative != "." && !paths.contains(&relative) {
            paths.push(relative);
        }
    }
    Ok(paths)
}

/// The committed content of `path`: `None` when Git is unavailable here,
/// `Some(None)` when the file is not in the last commit.
fn committed(root: &Path, path: &str) -> Option<Option<Vec<u8>>> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args([
                "--no-pager",
                "--no-optional-locks",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
    };
    let inside = git(&["rev-parse", "--verify", "--quiet", "HEAD"])?;
    if !inside.status.success() {
        // A repository without commits: every file is new.
        let repo = git(&["rev-parse", "--is-inside-work-tree"])?;
        return repo.status.success().then_some(None);
    }
    // `HEAD:./path` is relative to the project folder, which may be a
    // subfolder of the repository; `HEAD:path` would be from its root.
    let blob = git(&["cat-file", "blob", &format!("HEAD:./{path}")])?;
    Some(blob.status.success().then_some(blob.stdout))
}

fn baseline(store: &Store, ws: &Workspace, task: &str, path: &str) -> Result<Baseline> {
    if let Some(row) = checkpoint_row(store, ws, task, path)? {
        return Ok(row);
    }
    ensure!(
        task_paths(store, ws, task)?.iter().any(|p| p == path),
        "This task did not change {path}"
    );
    let bytes = committed(&ws.path, path).with_context(|| {
        format!("{path} was changed by a vendor agent and this project has no Git history to compare it with")
    })?;
    Ok(Baseline {
        source: "git",
        bytes,
        mode: None,
    })
}

fn status(before: &Option<Vec<u8>>, after: &Option<Vec<u8>>) -> &'static str {
    match (before, after) {
        (None, Some(_)) => "added",
        (Some(_), None) => "deleted",
        (Some(a), Some(b)) if a == b => "unchanged",
        (None, None) => "unchanged",
        _ => "modified",
    }
}

fn text(bytes: &Option<Vec<u8>>) -> Option<&str> {
    match bytes {
        None => Some(""),
        Some(b) if b.len() > MAX_DIFF_BYTES || b.contains(&0) => None,
        Some(b) => std::str::from_utf8(b).ok(),
    }
}

/// The current file, or `None` when it is missing. A file the review
/// cannot read (too large, not a regular file) is an error.
fn current(ws: &Workspace, path: &str) -> Result<Snapshot> {
    ws.snapshot(path)
}

/// `{path, status, source, added, removed, binary}` for one file.
fn summary(path: &str, base: &Baseline, now: &Snapshot) -> Value {
    let state = status(&base.bytes, &now.bytes);
    let (added, removed, binary) = match (text(&base.bytes), text(&now.bytes)) {
        (Some(old), Some(new)) => {
            let hunks = textdiff::hunks(old, new);
            (
                hunks.iter().map(|h| h.added()).sum::<usize>(),
                hunks.iter().map(|h| h.removed()).sum::<usize>(),
                false,
            )
        }
        _ => (0, 0, true),
    };
    json!({"path":path,"status":state,"source":base.source,"added":added,"removed":removed,"binary":binary})
}

/// The files this task changed, with their state now.
pub fn files(store: &Store, ws: &Workspace, task: &str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for path in task_paths(store, ws, task)? {
        let row = match (baseline(store, ws, task, &path), current(ws, &path)) {
            (Ok(base), Ok(now)) => summary(&path, &base, &now),
            (Err(error), _) | (_, Err(error)) => {
                json!({"path":path,"status":"unavailable","source":"none","added":0,"removed":0,"binary":false,"error":format!("{error:#}")})
            }
        };
        out.push(row);
    }
    Ok(out)
}

/// One file's hunks: `{path, status, source, binary, hunks: [{id, header,
/// lines}]}`.
pub fn file(store: &Store, ws: &Workspace, task: &str, path: &str) -> Result<Value> {
    let path = ws.relative(path)?.to_string_lossy().into_owned();
    let base = baseline(store, ws, task, &path)?;
    let now = current(ws, &path)?;
    let mut out = summary(&path, &base, &now);
    out["hunks"] = match (text(&base.bytes), text(&now.bytes)) {
        (Some(old), Some(new)) => json!(textdiff::hunks(old, new)
            .iter()
            .map(textdiff::Hunk::to_json)
            .collect::<Vec<_>>()),
        _ => json!([]),
    };
    out["hash"] = json!(now.hash.as_deref().unwrap_or("missing"));
    Ok(out)
}

/// What an undo did.
#[derive(Clone, Debug, Serialize)]
pub struct Undone {
    pub path: String,
    /// The hunk undone; `None` for the whole file.
    pub hunk: Option<String>,
}

/// Put one hunk (by id) or the whole file back as it was before the task.
pub fn undo(
    store: &Store,
    ws: &Workspace,
    task: &str,
    path: &str,
    hunk: Option<&str>,
) -> Result<Undone> {
    let path = ws.relative(path)?.to_string_lossy().into_owned();
    let base = baseline(store, ws, task, &path)?;
    let now = current(ws, &path)?;
    let now_hash = now.hash.clone().unwrap_or_else(|| "missing".into());
    let restored: Option<Vec<u8>> = match hunk {
        None => base.bytes.clone(),
        Some(id) => {
            let (Some(old), Some(new)) = (text(&base.bytes), text(&now.bytes)) else {
                bail!("{path} is binary or too large to undo in parts; undo the whole file");
            };
            let hunks = textdiff::hunks(old, new);
            let chosen = hunks
                .iter()
                .find(|h| h.id() == id)
                .context("This change is no longer in the file. Refresh the review")?;
            let reverted = textdiff::revert(new, chosen)
                .context("This change is no longer in the file. Refresh the review")?;
            // A file the task created and that is now empty again goes away.
            if base.bytes.is_none() && reverted.is_empty() {
                None
            } else {
                Some(reverted.into_bytes())
            }
        }
    };
    // Without a checkpoint, "not in the last commit" does not prove the
    // task created the file (it may be ignored or untracked and older than
    // the task), so it is never deleted on that evidence.
    ensure!(
        restored.is_some() || now.bytes.is_none() || base.source == "checkpoint",
        "{path} is not in the last commit and ShadowCode has no copy from before this task, so it cannot tell whether the task created it. Delete it yourself if it should go."
    );
    match &restored {
        Some(bytes) => {
            ws.write(&path, bytes, Some(&now_hash))?;
            if let Some(mode) = base.mode {
                ws.set_mode(&path, mode)?;
            }
        }
        None if now.bytes.is_some() => ws.delete(&path, Some(&now_hash))?,
        None => {}
    }
    if base.source == "checkpoint" {
        let after = restored
            .as_deref()
            .map(hash)
            .unwrap_or_else(|| "missing".into());
        store.execute(
            "UPDATE file_changes SET after_hash=?,observed_hash=? WHERE task_id=? AND workspace=? AND path=?",
            params![after, after, task, ws.path.to_string_lossy(), path],
        )?;
    }
    Ok(Undone {
        path,
        hunk: hunk.map(str::to_owned),
    })
}

/// A rewind that can be undone: the undo id and what it restored.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Rewind {
    pub undo_id: Option<String>,
    pub task_id: String,
    pub session_id: String,
    pub workspace: String,
    pub restored: Vec<String>,
}

/// Rewind a finished task's files, first recording them as they are so the
/// rewind can be undone (`undo_rewind`).
pub fn rewind(store: &Store, ws: &Workspace, task: &str, session_id: &str) -> Result<Rewind> {
    let undo_task = format!("rewind:{}", crate::id());
    let mut recorded = false;
    {
        let rows: Vec<(String, Option<Vec<u8>>)> = {
            let db = store.lock()?;
            let mut statement = db.prepare(
                "SELECT path,before_bytes FROM file_changes WHERE task_id=? AND workspace=? AND restored=0 ORDER BY id",
            )?;
            let rows = statement
                .query_map(params![task, ws.path.to_string_lossy()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (path, before) in rows {
            let now = ws.snapshot(&path)?;
            if now.bytes == before {
                continue;
            }
            checkpoint::record(store, ws, &undo_task, &path, &now, before.as_deref())?;
            recorded = true;
        }
    }
    let restored = match checkpoint::restore(store, ws, task) {
        Ok(paths) => paths,
        Err(error) => {
            forget(store, ws, &undo_task)?;
            return Err(error);
        }
    };
    let undo_id = if recorded && !restored.is_empty() {
        let id = undo_task.trim_start_matches("rewind:").to_owned();
        let record = Rewind {
            undo_id: Some(id.clone()),
            task_id: task.into(),
            session_id: session_id.into(),
            workspace: ws.path.to_string_lossy().into_owned(),
            restored: restored.clone(),
        };
        store.set_native_meta(&keys::rewind_undo(&id), &serde_json::to_string(&record)?)?;
        Some(id)
    } else {
        forget(store, ws, &undo_task)?;
        None
    };
    Ok(Rewind {
        undo_id,
        task_id: task.into(),
        session_id: session_id.into(),
        workspace: ws.path.to_string_lossy().into_owned(),
        restored,
    })
}

fn forget(store: &Store, ws: &Workspace, undo_task: &str) -> Result<()> {
    store.execute(
        "DELETE FROM file_changes WHERE task_id=? AND workspace=?",
        params![undo_task, ws.path.to_string_lossy()],
    )?;
    Ok(())
}

/// The recorded rewind for an undo id.
pub fn rewind_record(store: &Store, undo_id: &str) -> Result<Rewind> {
    ensure!(
        !undo_id.is_empty()
            && undo_id.len() <= 64
            && undo_id.chars().all(|c| c.is_ascii_alphanumeric()),
        "Unknown rewind"
    );
    let text = store
        .native_meta(&keys::rewind_undo(undo_id))?
        .context("This rewind can no longer be undone")?;
    Ok(serde_json::from_str(&text)?)
}

/// Put back the files a rewind restored, as they were just before it. The
/// task can be rewound again afterwards. Files changed since the rewind
/// stop the undo before anything is written.
pub fn undo_rewind(store: &Store, ws: &Workspace, undo_id: &str) -> Result<Rewind> {
    let record = rewind_record(store, undo_id)?;
    ensure!(
        Path::new(&record.workspace) == ws.path,
        "This rewind belongs to another project"
    );
    let undo_task = format!("rewind:{undo_id}");
    let put_back = checkpoint::restore(store, ws, &undo_task).map_err(|error| {
        anyhow::anyhow!(
            "{}",
            format!("{error:#}").replace("Cannot rewind", "Cannot undo the rewind")
        )
    })?;
    for path in &put_back {
        let now = ws.snapshot(path)?.hash.unwrap_or_else(|| "missing".into());
        store.execute(
            "UPDATE file_changes SET restored=0,after_hash=?,observed_hash=? WHERE task_id=? AND workspace=? AND path=?",
            params![now, now, record.task_id, ws.path.to_string_lossy(), path],
        )?;
    }
    forget(store, ws, &undo_task)?;
    store.execute(
        "DELETE FROM native_meta WHERE key=?",
        params![keys::rewind_undo(undo_id)],
    )?;
    Ok(Rewind {
        restored: put_back,
        ..record
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup() -> (tempfile::TempDir, Store, Workspace, String, String) {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        fs::create_dir(&project).unwrap();
        let store = Store::open(&root.path().join("db")).unwrap();
        let session = store.create_session(&project, "mock", "").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let task = store.create_task(&session, "edit").unwrap();
        let ws = Workspace::open(&project).unwrap();
        (root, store, ws, session, task)
    }

    /// Change a file as a native tool does: checkpoint, then write.
    fn change(store: &Store, ws: &Workspace, task: &str, path: &str, after: Option<&str>) {
        let before = ws.snapshot(path).unwrap();
        checkpoint::record(store, ws, task, path, &before, after.map(str::as_bytes)).unwrap();
        match after {
            Some(text) => {
                ws.write(path, text.as_bytes(), None).unwrap();
            }
            None => ws.delete(path, None).unwrap(),
        }
    }

    const OLD: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
    const NEW: &str = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nK\nl\n";

    #[test]
    fn lists_only_this_tasks_files_with_counts() {
        let (_root, store, ws, _session, task) = setup();
        fs::write(ws.path.join("keep.txt"), OLD).unwrap();
        fs::write(ws.path.join("untouched.txt"), "x\n").unwrap();
        change(&store, &ws, &task, "keep.txt", Some(NEW));
        change(&store, &ws, &task, "new.txt", Some("hello\n"));
        let list = files(&store, &ws, &task).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["path"], "keep.txt");
        assert_eq!(list[0]["status"], "modified");
        assert_eq!(list[0]["added"], 2);
        assert_eq!(list[0]["removed"], 2);
        assert_eq!(list[1]["status"], "added");
        let detail = file(&store, &ws, &task, "keep.txt").unwrap();
        assert_eq!(detail["hunks"].as_array().unwrap().len(), 2);
        assert!(file(&store, &ws, &task, "untouched.txt").is_err());
    }

    #[test]
    fn undo_one_hunk_then_the_file_and_rewind_still_works() {
        let (_root, store, ws, session, task) = setup();
        fs::write(ws.path.join("keep.txt"), OLD).unwrap();
        change(&store, &ws, &task, "keep.txt", Some(NEW));
        let detail = file(&store, &ws, &task, "keep.txt").unwrap();
        let second = detail["hunks"][1]["id"].as_str().unwrap().to_owned();
        undo(&store, &ws, &task, "keep.txt", Some(&second)).unwrap();
        let text = fs::read_to_string(ws.path.join("keep.txt")).unwrap();
        assert!(text.contains("B\n") && text.contains("\nk\n"));
        // The same hunk id is gone now.
        assert!(undo(&store, &ws, &task, "keep.txt", Some(&second)).is_err());
        // Rewind accepts the partly undone file.
        let rewound = rewind(&store, &ws, &task, &session).unwrap();
        assert_eq!(rewound.restored, ["keep.txt"]);
        assert_eq!(fs::read_to_string(ws.path.join("keep.txt")).unwrap(), OLD);
    }

    #[test]
    fn undo_a_created_file_removes_it() {
        let (_root, store, ws, _session, task) = setup();
        change(&store, &ws, &task, "new.txt", Some("hello\n"));
        undo(&store, &ws, &task, "new.txt", None).unwrap();
        assert!(!ws.path.join("new.txt").exists());
        let list = files(&store, &ws, &task).unwrap();
        assert_eq!(list[0]["status"], "unchanged");
        // Undoing its only hunk removes it too.
        change(&store, &ws, &task, "other.txt", Some("x\n"));
        let hunk = file(&store, &ws, &task, "other.txt").unwrap()["hunks"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        undo(&store, &ws, &task, "other.txt", Some(&hunk)).unwrap();
        assert!(!ws.path.join("other.txt").exists());
    }

    #[test]
    fn undo_refuses_a_file_changed_since_the_diff() {
        let (_root, store, ws, _session, task) = setup();
        fs::write(ws.path.join("keep.txt"), OLD).unwrap();
        change(&store, &ws, &task, "keep.txt", Some(NEW));
        let hunk = file(&store, &ws, &task, "keep.txt").unwrap()["hunks"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        fs::write(ws.path.join("keep.txt"), "rewritten\n").unwrap();
        assert!(undo(&store, &ws, &task, "keep.txt", Some(&hunk)).is_err());
        assert_eq!(
            fs::read_to_string(ws.path.join("keep.txt")).unwrap(),
            "rewritten\n"
        );
    }

    #[test]
    fn rewind_can_be_undone_and_redone() {
        let (_root, store, ws, session, task) = setup();
        fs::write(ws.path.join("keep.txt"), OLD).unwrap();
        change(&store, &ws, &task, "keep.txt", Some(NEW));
        change(&store, &ws, &task, "new.txt", Some("hello\n"));
        let rewound = rewind(&store, &ws, &task, &session).unwrap();
        let id = rewound.undo_id.clone().unwrap();
        assert_eq!(rewound.restored.len(), 2);
        assert_eq!(fs::read_to_string(ws.path.join("keep.txt")).unwrap(), OLD);
        assert!(!ws.path.join("new.txt").exists());
        assert_eq!(rewind_record(&store, &id).unwrap().task_id, task);

        let undone = undo_rewind(&store, &ws, &id).unwrap();
        assert_eq!(undone.restored.len(), 2);
        assert_eq!(fs::read_to_string(ws.path.join("keep.txt")).unwrap(), NEW);
        assert_eq!(
            fs::read_to_string(ws.path.join("new.txt")).unwrap(),
            "hello\n"
        );
        assert!(rewind_record(&store, &id).is_err(), "used once");
        // The task can be rewound again.
        let again = rewind(&store, &ws, &task, &session).unwrap();
        assert_eq!(again.restored.len(), 2);
        assert_eq!(fs::read_to_string(ws.path.join("keep.txt")).unwrap(), OLD);
    }

    #[test]
    fn undo_rewind_stops_when_files_changed_after_it() {
        let (_root, store, ws, session, task) = setup();
        fs::write(ws.path.join("keep.txt"), OLD).unwrap();
        change(&store, &ws, &task, "keep.txt", Some(NEW));
        let id = rewind(&store, &ws, &task, &session)
            .unwrap()
            .undo_id
            .unwrap();
        fs::write(ws.path.join("keep.txt"), "edited by hand\n").unwrap();
        let error = undo_rewind(&store, &ws, &id).unwrap_err().to_string();
        assert!(error.contains("Cannot undo the rewind"), "{error}");
        assert_eq!(
            fs::read_to_string(ws.path.join("keep.txt")).unwrap(),
            "edited by hand\n"
        );
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    /// A vendor-reported file without a checkpoint row is compared with the
    /// last commit relative to the project folder (a subfolder of the
    /// repository here), and a file that is not in the commit is never
    /// deleted on that evidence alone.
    #[test]
    fn vendor_files_without_checkpoints_compare_from_the_project_folder() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let project = repo.join("app");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("a.txt"), "one\n").unwrap();
        fs::write(repo.join(".gitignore"), ".env\n").unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "base"]);
        fs::write(project.join(".env"), "SECRET=1\n").unwrap();
        let store = Store::open(&root.path().join("db")).unwrap();
        let session = store.create_session(&project, "mock", "").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let task = store.create_task(&session, "edit").unwrap();
        let ws = Workspace::open(&project).unwrap();
        store
            .add_event(
                "files.changed",
                &json!({"paths":["a.txt", ".env"]}),
                Some(&session),
                Some(&task),
            )
            .unwrap();
        fs::write(project.join("a.txt"), "two\n").unwrap();
        fs::write(project.join(".env"), "SECRET=2\n").unwrap();
        let list = files(&store, &ws, &task).unwrap();
        assert_eq!(list[0]["path"], "a.txt");
        assert_eq!(list[0]["status"], "modified", "{list:?}");
        undo(&store, &ws, &task, "a.txt", None).unwrap();
        assert_eq!(fs::read_to_string(project.join("a.txt")).unwrap(), "one\n");
        let error = undo(&store, &ws, &task, ".env", None).unwrap_err();
        assert!(error.to_string().contains("cannot tell"), "{error}");
        assert_eq!(
            fs::read_to_string(project.join(".env")).unwrap(),
            "SECRET=2\n"
        );
    }

    #[test]
    fn a_failed_rewind_leaves_no_undo_record() {
        let (_root, store, ws, session, task) = setup();
        fs::write(ws.path.join("keep.txt"), OLD).unwrap();
        change(&store, &ws, &task, "keep.txt", Some(NEW));
        fs::write(ws.path.join("keep.txt"), "changed after\n").unwrap();
        assert!(rewind(&store, &ws, &task, &session).is_err());
        let left = store
            .query(
                "SELECT COUNT(*) AS n FROM file_changes WHERE task_id LIKE 'rewind:%'",
                [],
            )
            .unwrap();
        assert_eq!(left[0]["n"], 0);
    }
}

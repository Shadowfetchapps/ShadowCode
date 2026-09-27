//! Copy a reviewed dirty snapshot without stashing or resetting the source.
use super::{git, AppPaths, Record, Workspace};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;
#[derive(Clone, Debug, Serialize)]
pub struct Untracked {
    pub path: String,
    pub bytes: usize,
    pub hash: String,
    pub mode: u32,
}
#[derive(Clone, Debug, Serialize)]
pub struct Review {
    pub source: PathBuf,
    pub head: String,
    pub staged_diff: String,
    pub unstaged_diff: String,
    pub untracked: Vec<Untracked>,
    pub intent_to_add: Vec<String>,
    pub hash: String,
}
struct Capture {
    review: Review,
    files: Vec<Vec<u8>>,
}
async fn capture(source: &Path, cancel: CancellationToken) -> Result<Capture> {
    let workspace = Workspace::open(source)?;
    let top = git(source, &["rev-parse", "--show-toplevel"], cancel.clone()).await?;
    ensure!(
        Path::new(&top).canonicalize()? == workspace.path,
        "Open the repository root before copying changes"
    );
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        let location = git(
            source,
            &["rev-parse", "--path-format=absolute", "--git-path", marker],
            cancel.clone(),
        )
        .await?;
        ensure!(
            !Path::new(&location).try_exists()?,
            "Finish the existing Git operation before copying changes"
        );
    }
    let unmerged = git(source, &["ls-files", "--unmerged", "-z"], cancel.clone()).await?;
    ensure!(
        unmerged.is_empty(),
        "Resolve index conflicts before copying changes"
    );
    let head = git(source, &["rev-parse", "--verify", "HEAD"], cancel.clone()).await?;
    let raw = git(
        source,
        &[
            "diff",
            "--raw",
            "--no-abbrev",
            "--ignore-submodules=none",
            "HEAD",
            "--",
        ],
        cancel.clone(),
    )
    .await?;
    ensure!(
        !raw.lines()
            .any(|line| line.starts_with(":160000 ")
                || line.split_whitespace().nth(1) == Some("160000")),
        "Copy submodule changes separately; nested repository edits are not included"
    );
    let staged_diff = git(
        source,
        &[
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "HEAD",
            "--",
        ],
        cancel.clone(),
    )
    .await?;
    let unstaged_diff = git(
        source,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--",
        ],
        cancel.clone(),
    )
    .await?;
    let intent_to_add = git(
        source,
        &["diff", "--name-only", "--diff-filter=A", "-z", "--"],
        cancel.clone(),
    )
    .await?
    .split('\0')
    .filter(|s| !s.is_empty())
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let names = git(
        source,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        cancel.clone(),
    )
    .await?;
    let mut untracked = vec![];
    let mut files = vec![];
    let mut total = 0usize;
    for name in names.split('\0').filter(|s| !s.is_empty()) {
        ensure!(!cancel.is_cancelled(), "Copy inspection cancelled");
        ensure!(
            untracked.len() < 256,
            "Copy at most 256 untracked files at once"
        );
        let relative = workspace.writable(name)?;
        let (bytes, length) = workspace.inspect(name, crate::workspace::MAX_FILE_BYTES + 1)?;
        ensure!(
            length <= crate::workspace::MAX_FILE_BYTES as u64
                && bytes.len() <= crate::workspace::MAX_FILE_BYTES,
            "Untracked file exceeds the 4 MB copy limit"
        );
        let meta = fs::symlink_metadata(workspace.path.join(relative))?;
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "Only regular untracked files can be copied"
        );
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = 0o644;
        total += bytes.len();
        ensure!(
            total <= 32_000_000,
            "Untracked contents exceed the 32 MB copy limit"
        );
        untracked.push(Untracked {
            path: name.into(),
            bytes: bytes.len(),
            hash: crate::workspace::hash(&bytes),
            mode,
        });
        files.push(bytes);
    }
    ensure!(
        !staged_diff.is_empty() || !unstaged_diff.is_empty() || !untracked.is_empty(),
        "No uncommitted changes to copy"
    );
    let mut review = Review {
        source: workspace.path,
        head,
        staged_diff,
        unstaged_diff,
        untracked,
        intent_to_add,
        hash: String::new(),
    };
    review.hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&review)?));
    Ok(Capture { review, files })
}
pub async fn review(source: &Path, cancel: CancellationToken) -> Result<Review> {
    let first = capture(source, cancel.clone()).await?;
    let second = capture(source, cancel).await?;
    ensure!(
        first.review.hash == second.review.hash,
        "Source changed during inspection; inspect it again"
    );
    Ok(second.review)
}
pub async fn copy<R>(
    paths: &AppPaths,
    source: &Path,
    expected_hash: &str,
    cancel: CancellationToken,
    reserve: impl FnOnce(&Path) -> Result<R>,
) -> Result<Record> {
    let ownership = super::locks::mutation(source, &cancel).await?;
    let snapshot = capture(source, cancel.clone()).await?;
    ensure!(
        snapshot.review.hash == expected_hash,
        "Source changed; review its changes again before copying"
    );
    ensure!(
        capture(source, cancel.clone()).await?.review.hash == expected_hash,
        "Source changed during capture; review it again"
    );
    let mut record = super::create_locked(
        paths,
        source,
        &snapshot.review.head,
        false,
        &ownership,
        cancel.clone(),
    )
    .await?;
    let (records, _) = super::roots(paths)?;
    record.state = "copying".into();
    record.detail =
        "Copying reviewed uncommitted changes; source checkout is retained untouched".into();
    super::save(&records, &record)?;
    let result: Result<()> = async {
        let _reservation = reserve(&record.path)?;
        let temporary = tempfile::tempdir_in(&paths.data)?;
        for (name, patch, staged) in [
            ("staged.patch", &snapshot.review.staged_diff, true),
            ("unstaged.patch", &snapshot.review.unstaged_diff, false),
        ] {
            if patch.is_empty() {
                continue;
            }
            let file = temporary.path().join(name);
            // The Git helper trims trailing newlines; patches require the final LF.
            super::paths::atomic_write(&file, format!("{patch}\n\n").as_bytes(), true)?;
            let mut args = vec!["apply", "--binary", "--whitespace=nowarn"];
            if staged {
                args.push("--index");
            }
            args.extend(["--", file.to_str().context("Patch path must be UTF-8")?]);
            git(&record.path, &args, cancel.clone()).await?;
        }
        for name in &snapshot.review.intent_to_add {
            git(
                &record.path,
                &["add", "--intent-to-add", "--", name],
                cancel.clone(),
            )
            .await?;
        }
        let destination = Workspace::open(&record.path)?;
        for (entry, bytes) in snapshot.review.untracked.iter().zip(&snapshot.files) {
            ensure!(!cancel.is_cancelled(), "Copy cancelled");
            destination.write(&entry.path, bytes, Some("missing"))?;
            destination.set_mode(&entry.path, entry.mode)?;
        }
        // Applying patches must reproduce the reviewed staging split and bytes.
        let applied = capture(&record.path, cancel.clone()).await?;
        ensure!(
            applied.review.head == snapshot.review.head
                && applied.review.staged_diff == snapshot.review.staged_diff
                && applied.review.unstaged_diff == snapshot.review.unstaged_diff
                && applied.review.intent_to_add == snapshot.review.intent_to_add
                && serde_json::to_vec(&applied.review.untracked)?
                    == serde_json::to_vec(&snapshot.review.untracked)?,
            "Copied checkout differs from the reviewed snapshot; preserve it for inspection"
        );
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            record.state = "ready".into();
            record.detail="Reviewed staged, unstaged and untracked changes copied. Source checkout is unchanged; ignored files were not copied.".into();
            super::save(&records, &record)?;
            Ok(record)
        }
        Err(error) => {
            record.state = "needs_attention".into();
            record.detail=format!("Copy stopped: {error:#}. Source checkout is untouched; partial destination retained for inspection.").chars().take(2000).collect();
            super::save(&records, &record)?;
            Err(error.context(format!(
                "Partial copy retained at {}",
                record.path.display()
            )))
        }
    }
}

//! Restore only missing managed Git connection files; never rebuild an index.
use super::{git, read_record, roots, save, AppPaths, Record};
use crate::paths;
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize)]
pub struct Review {
    pub record: Record,
    pub administrative_directory: PathBuf,
    pub head: String,
    pub checkout_pointer: Option<String>,
    pub registration_pointer: Option<String>,
    pub warning: String,
    pub hash: String,
}
fn directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink() && path.canonicalize()? == path,
        "Repair requires the original real directory at {}",
        path.display()
    );
    Ok(())
}
fn text(path: &Path) -> Result<Option<String>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        file.metadata()?.is_file(),
        "Git connection metadata must be a regular file"
    );
    let mut bytes = vec![];
    file.take(16_385).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16_384,
        "Git connection metadata exceeds 16 KB"
    );
    Ok(Some(String::from_utf8(bytes)?))
}
pub async fn review(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    cancel: CancellationToken,
) -> Result<Review> {
    let record = read_record(paths, source, id)?;
    directory(&record.path)?;
    directory(&record.common_directory)?;
    let common = git(
        source,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cancel.clone(),
    )
    .await?;
    ensure!(
        Path::new(&common).canonicalize()? == record.common_directory,
        "Source repository identity changed"
    );
    let administrative_directory = record.common_directory.join("worktrees").join(id);
    directory(&administrative_directory)?;
    ensure!(
        fs::symlink_metadata(administrative_directory.join("locked"))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "Unlock the worktree deliberately before repairing its connection"
    );
    ensure!(
        text(&administrative_directory.join("commondir"))?
            .as_deref()
            .map(str::trim_end)
            == Some("../.."),
        "Retained common-directory identity changed; inspect metadata manually"
    );
    let index = fs::symlink_metadata(administrative_directory.join("index"))
        .context("Retained index is missing; preserve the checkout for manual recovery")?;
    ensure!(
        index.is_file() && !index.file_type().is_symlink(),
        "Retained index must be a regular file"
    );
    let reference = format!("ref: refs/heads/{}", record.branch);
    ensure!(
        text(&administrative_directory.join("HEAD"))?
            .as_deref()
            .map(str::trim_end)
            == Some(&reference),
        "Retained branch identity changed; inspect metadata manually"
    );
    let head = git(
        source,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}^{{commit}}", record.branch),
        ],
        cancel,
    )
    .await?;
    let checkout_pointer = text(&record.path.join(".git"))?;
    let registration_pointer = text(&administrative_directory.join("gitdir"))?;
    let expected_checkout = format!("gitdir: {}", administrative_directory.display());
    let expected_registration = record.path.join(".git").to_string_lossy().into_owned();
    ensure!(
        checkout_pointer
            .as_deref()
            .is_none_or(|value| value.trim_end() == expected_checkout),
        "Checkout points somewhere else; preserve and inspect that connection manually"
    );
    ensure!(
        registration_pointer
            .as_deref()
            .is_none_or(|value| value.trim_end() == expected_registration),
        "Registration points somewhere else; preserve and inspect that connection manually"
    );
    ensure!(
        checkout_pointer.is_none() || registration_pointer.is_none(),
        "Both Git connections are already intact"
    );
    let mut review = Review {record, administrative_directory, head, checkout_pointer, registration_pointer, warning: "Restores only missing connection files at the original managed location. Existing files, staging, commits and branch remain unchanged. Lost indexes, moved checkouts and conflicting connections require separate manual inspection.".into(), hash: String::new()};
    review.hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&review)?));
    Ok(review)
}
pub async fn apply(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    hash: &str,
    cancel: CancellationToken,
) -> Result<Record> {
    let _ownership = super::locks::mutation(source, &cancel).await?;
    let reviewed = review(paths, source, id, cancel.clone()).await?;
    ensure!(
        reviewed.hash == hash,
        "Repair state changed; review it again"
    );
    let (records, _) = roots(paths)?;
    let journals = records.join("repairs");
    paths::private_directory(&journals)?;
    let journal = journals.join(format!("{}-{}.json", id, crate::id()));
    paths::atomic_write(&journal, &serde_json::to_vec_pretty(&reviewed)?, true)?;
    let mut record = reviewed.record;
    record.state = "repairing".into();
    record.detail = format!(
        "Repair journal: {}. Only missing Git connection files are restored.",
        journal.display()
    );
    save(&records, &record)?;
    let result: Result<()> = async {
        ensure!(!cancel.is_cancelled(), "Worktree repair cancelled");
        // Create without clobbering any connection another writer has restored.
        for (path, value, missing) in [
            (
                record.path.join(".git"),
                format!("gitdir: {}\n", reviewed.administrative_directory.display()),
                reviewed.checkout_pointer.is_none(),
            ),
            (
                reviewed.administrative_directory.join("gitdir"),
                format!("{}\n", record.path.join(".git").display()),
                reviewed.registration_pointer.is_none(),
            ),
        ] {
            if missing {
                use std::io::Write;
                let mut temporary = tempfile::NamedTempFile::new_in(
                    path.parent().context("Missing connection parent")?,
                )?;
                temporary.write_all(value.as_bytes())?;
                temporary.as_file().sync_all()?;
                temporary
                    .persist_noclobber(&path)
                    .map_err(|error| error.error)?;
                fs::File::open(path.parent().unwrap())?.sync_all()?;
            }
        }
        let top = git(
            &record.path,
            &["rev-parse", "--show-toplevel"],
            cancel.clone(),
        )
        .await?;
        ensure!(
            Path::new(&top).canonicalize()? == record.path,
            "Repaired checkout root differs"
        );
        let common = git(
            &record.path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            cancel,
        )
        .await?;
        ensure!(
            Path::new(&common).canonicalize()? == record.common_directory,
            "Repaired repository identity differs"
        );
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            record.state = "ready".into();
            record.detail = format!(
                "Git connection restored; files and index preserved. Repair journal: {}",
                journal.display()
            );
            save(&records, &record)?;
            Ok(record)
        }
        Err(error) => {
            record.state = "needs_attention".into();
            record.detail = format!(
                "Repair stopped: {error:#}. Preserve all files and inspect journal {}",
                journal.display()
            )
            .chars()
            .take(2000)
            .collect();
            save(&records, &record)?;
            Err(error)
        }
    }
}

//! Cooperative worktree mutation ownership. Order: optional Compare ownership,
//! repository local/advisory lock, then short profile admission. Never acquire
//! repository ownership while holding profile admission, and never unlink locks.
use super::{git, AppPaths, Record, Workspace};
use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Repository,
    Admission,
}
type Registry = BTreeMap<(Kind, PathBuf), Weak<AsyncMutex<()>>>;
static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn keyed(kind: Kind, path: &Path) -> Result<Arc<AsyncMutex<()>>> {
    let mut registry = REGISTRY
        .lock()
        .map_err(|_| anyhow::anyhow!("Worktree lock registry poisoned"))?;
    registry.retain(|_, weak| weak.strong_count() > 0);
    let key = (kind, path.to_owned());
    let lock = registry
        .get(&key)
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| Arc::new(AsyncMutex::new(())));
    registry.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

struct Advisory {
    file: fs::File,
    directory: fs::File,
    path: PathBuf,
}
impl Advisory {
    fn open(directory: &Path, name: &str, busy: &str) -> Result<Self> {
        let mut parent = fs::OpenOptions::new();
        parent.read(true);
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            parent.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        ensure!(
            directory.canonicalize()? == directory,
            "Worktree lock directory changed"
        );
        let parent = parent.open(directory)?;
        ensure!(
            parent.metadata()?.is_dir(),
            "Worktree lock parent must be a directory"
        );
        let path = directory.join(name);
        let file = options.open(&path)?;
        let lock = Self {
            file,
            directory: parent,
            path,
        };
        lock.validate()?;
        lock.file
            .try_lock_exclusive()
            .with_context(|| busy.to_owned())?;
        lock.validate()?;
        Ok(lock)
    }
    fn validate(&self) -> Result<()> {
        let opened = self.file.metadata()?;
        let named = fs::symlink_metadata(&self.path)?;
        let parent = fs::symlink_metadata(self.path.parent().context("Missing lock parent")?)?;
        ensure!(
            opened.is_file()
                && named.is_file()
                && !named.file_type().is_symlink()
                && parent.is_dir()
                && !parent.file_type().is_symlink()
                && self.directory.metadata()?.is_dir(),
            "Unsafe worktree advisory lock path"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let pinned = self.directory.metadata()?;
            ensure!(
                opened.uid() == unsafe { libc::geteuid() }
                    && opened.nlink() == 1
                    && opened.mode() & 0o077 == 0
                    && opened.dev() == named.dev()
                    && opened.ino() == named.ino()
                    && pinned.dev() == parent.dev()
                    && pinned.ino() == parent.ino(),
                "Worktree advisory lock identity changed or is unsafe"
            );
        }
        Ok(())
    }
}

pub(super) struct Mutation {
    // File first: release the OS lock before another local waiter proceeds.
    advisory: Advisory,
    _local: OwnedMutexGuard<()>,
    common: PathBuf,
    source: PathBuf,
}
impl Mutation {
    pub(super) fn validate(&self, source: &Path, common: &Path) -> Result<()> {
        ensure!(
            Workspace::open(source)?.path == self.source && common == self.common,
            "Worktree mutation ownership belongs to another repository"
        );
        self.advisory.validate()
    }
}
async fn common(source: &Path, cancel: &CancellationToken) -> Result<PathBuf> {
    let common = git(
        source,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cancel.clone(),
    )
    .await?;
    Ok(Path::new(&common).canonicalize()?)
}
pub(super) async fn mutation(source: &Path, cancel: &CancellationToken) -> Result<Mutation> {
    let source = Workspace::open(source)?.path;
    let directory = common(&source, cancel).await?;
    let lock = keyed(Kind::Repository, &directory)?;
    let local = tokio::select! {biased; _=cancel.cancelled()=>anyhow::bail!("Worktree mutation cancelled"), guard=lock.lock_owned()=>guard};
    ensure!(!cancel.is_cancelled(), "Worktree mutation cancelled");
    // Retain the existing disposal lock inode/name so older disposal callers
    // still conflict. Other old-version mutators remain outside this protocol.
    let advisory = Advisory::open(
        &directory,
        "shadowcode-disposal.lock",
        "Another process owns worktree cleanup or mutation; retry later",
    )?;
    ensure!(
        common(&source, cancel).await? == directory,
        "Source repository identity changed while waiting for ownership"
    );
    let result = Mutation {
        advisory,
        _local: local,
        common: directory,
        source,
    };
    result.advisory.validate()?;
    Ok(result)
}

/// A durable creating record is the slot reservation. Failed/cancelled creates
/// remain counted as recovery material; restart never silently frees their slot.
/// This guard is dropped before the first Git mutation or cleanup inspection.
pub(super) async fn reserve_record(
    paths: &AppPaths,
    records: &Path,
    record: &Record,
    ownership: &Mutation,
    cancel: &CancellationToken,
) -> Result<()> {
    ownership.validate(&record.source, &record.common_directory)?;
    let (expected, _) = super::roots(paths)?;
    ensure!(
        expected == records,
        "Worktree admission belongs to another profile"
    );
    let directory = records.canonicalize()?;
    let lock = keyed(Kind::Admission, &directory)?;
    let _local = tokio::select! {biased; _=cancel.cancelled()=>anyhow::bail!("Worktree admission cancelled"), guard=lock.lock_owned()=>guard};
    ensure!(!cancel.is_cancelled(), "Worktree admission cancelled");
    let advisory = Advisory::open(
        &directory,
        ".admission.lock",
        "Another process owns worktree admission; retry later",
    )?;
    ensure!(
        records.canonicalize()? == directory,
        "Worktree inventory changed during admission"
    );
    let mut count = 0;
    let mut entries = 0;
    for entry in fs::read_dir(&directory)? {
        entries += 1;
        ensure!(
            entries <= 256,
            "Worktree inventory has too many entries; review retained temporary records"
        );
        let entry = entry?;
        if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
            count += 1;
            ensure!(
                count < 64,
                "At most 64 managed worktree records are allowed"
            );
        }
    }
    advisory.validate()?;
    super::save(records, record)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn advisory_refuses_symlinks_hardlinks_and_replaced_ownership() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("repository");
        fs::create_dir(&directory).unwrap();
        let target = root.path().join("foreign");
        fs::write(&target, b"preserve").unwrap();
        symlink(&target, directory.join("symlink")).unwrap();
        assert!(Advisory::open(&directory, "symlink", "busy").is_err());
        fs::hard_link(&target, directory.join("hardlink")).unwrap();
        assert!(Advisory::open(&directory, "hardlink", "busy").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"preserve");
        let lock = Advisory::open(&directory, "owned", "busy").unwrap();
        fs::rename(directory.join("owned"), directory.join("preserved-owned")).unwrap();
        fs::write(directory.join("owned"), b"replacement").unwrap();
        assert!(lock.validate().is_err());
        assert_eq!(fs::read(directory.join("owned")).unwrap(), b"replacement");
        drop(lock);
        let lock = Advisory::open(&directory, "parent-check", "busy").unwrap();
        fs::rename(&directory, root.path().join("preserved-repository")).unwrap();
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("parent-check"), b"replacement parent").unwrap();
        assert!(lock.validate().is_err());
        assert_eq!(
            fs::read(directory.join("parent-check")).unwrap(),
            b"replacement parent"
        );
    }
    fn repository(path: &Path) {
        fs::create_dir(path).unwrap();
        command(path, &["init", "-q"]);
        fs::write(path.join("tracked.txt"), b"original\n").unwrap();
        command(path, &["add", "tracked.txt"]);
        command(path, &["commit", "-qm", "Base"]);
    }
    fn command(path: &Path, args: &[&str]) {
        let result = std::process::Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Lock Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn common_directory_aliases_serialize_and_cancelled_waiters_do_not_block_other_repositories(
    ) {
        use std::time::{Duration, Instant};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let second = root.path().join("second");
        repository(&source);
        repository(&second);
        let alias = root.path().join("source-alias");
        symlink(&source, &alias).unwrap();
        let linked = root.path().join("linked-checkout");
        command(
            &source,
            &[
                "worktree",
                "add",
                "--detach",
                linked.to_str().unwrap(),
                "HEAD",
            ],
        );
        let cancel = CancellationToken::new();
        let common_dir = common(&source, &cancel).await.unwrap();
        let held = mutation(&source, &cancel).await.unwrap();
        let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
        for waiting_source in [&alias, &linked] {
            assert_eq!(common(waiting_source, &cancel).await.unwrap(), common_dir);
            let waiting_source = waiting_source.to_owned();
            let waiting_cancel = CancellationToken::new();
            let token = waiting_cancel.clone();
            let waiter =
                tokio::spawn(async move { mutation(&waiting_source, &token).await.map(|_| ()) });
            // Wait until the actual mutation future has retained the same keyed
            // Arc as the held owner. This avoids calling an arbitrary sleep a
            // proof that a slow Git discovery reached the local lock queue.
            let started = Instant::now();
            loop {
                let waiting = REGISTRY
                    .lock()
                    .unwrap()
                    .get(&(Kind::Repository, common_dir.clone()))
                    .is_some_and(|weak| weak.strong_count() >= 2);
                if waiting {
                    break;
                }
                assert!(
                    !waiter.is_finished(),
                    "Alias bypassed shared local ownership"
                );
                assert!(
                    started.elapsed() < Duration::from_secs(3),
                    "Waiter did not reach common-directory lock"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(!waiter.is_finished());
            let independent = tokio::time::timeout(
                Duration::from_secs(2),
                super::super::create(&paths, &second, "HEAD", CancellationToken::new()),
            )
            .await
            .expect("Unrelated repository was blocked by an alias waiter")
            .unwrap();
            assert_eq!(
                fs::read(independent.path.join("tracked.txt")).unwrap(),
                b"original\n"
            );
            waiting_cancel.cancel();
            let error = tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(
                error.to_string().contains("Worktree mutation cancelled"),
                "{error:#}"
            );
            held.validate(&source, &common_dir).unwrap();
            super::super::dispose(&paths, &second, &independent.id, CancellationToken::new())
                .await
                .unwrap();
        }
        drop(held);
        for waiting_source in [&alias, &linked] {
            let acquired =
                tokio::time::timeout(Duration::from_secs(1), mutation(waiting_source, &cancel))
                    .await
                    .unwrap()
                    .unwrap();
            acquired.advisory.validate().unwrap();
            drop(acquired);
        }
        assert_eq!(fs::read(source.join("tracked.txt")).unwrap(), b"original\n");
        assert_eq!(fs::read(linked.join("tracked.txt")).unwrap(), b"original\n");
        assert!(super::super::list(&paths, &second).unwrap().is_empty());
    }
}

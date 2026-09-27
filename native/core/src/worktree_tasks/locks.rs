//! Task metadata ownership. Mutators acquire repository -> record -> core
//! worktree ownership. Readers acquire only record ownership. This module never
//! acquires the core mutation mutex or advisory file, so a task may safely call
//! create/dispose/release while it owns this distinct outer guard.
use crate::{store::Store, worktrees};
use anyhow::{ensure, Result};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Repository(PathBuf),
    Record(PathBuf, String),
}
static REGISTRY: LazyLock<Mutex<BTreeMap<Key, Weak<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn keyed(key: Key) -> Result<Arc<AsyncMutex<()>>> {
    let mut registry = REGISTRY
        .lock()
        .map_err(|_| anyhow::anyhow!("Worktree task lock registry poisoned"))?;
    registry.retain(|_, weak| weak.strong_count() > 0);
    let lock = registry
        .get(&key)
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| Arc::new(AsyncMutex::new(())));
    registry.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}
async fn acquire(key: Key, cancel: &CancellationToken) -> Result<OwnedMutexGuard<()>> {
    let lock = keyed(key)?;
    let guard = tokio::select! {
        biased;
        _ = cancel.cancelled() => anyhow::bail!("Worktree task operation cancelled"),
        guard = lock.lock_owned() => guard,
    };
    ensure!(!cancel.is_cancelled(), "Worktree task operation cancelled");
    Ok(guard)
}

pub(super) struct Repository {
    pub(super) common: PathBuf,
    _guard: OwnedMutexGuard<()>,
}
pub(super) async fn repository(source: &Path, cancel: &CancellationToken) -> Result<Repository> {
    let common = worktrees::task_common_directory(source, cancel).await?;
    let guard = acquire(Key::Repository(common.clone()), cancel).await?;
    ensure!(
        worktrees::task_common_directory(source, cancel).await? == common,
        "Worktree task source repository changed while waiting for ownership"
    );
    Ok(Repository {
        common,
        _guard: guard,
    })
}
pub(super) async fn record(
    store: &Store,
    id: &str,
    cancel: &CancellationToken,
) -> Result<OwnedMutexGuard<()>> {
    super::valid_id(id)?;
    acquire(
        Key::Record(store.path.canonicalize()?, id.to_owned()),
        cancel,
    )
    .await
}

#[cfg(test)]
pub(super) fn repository_references(common: &Path) -> usize {
    REGISTRY
        .lock()
        .unwrap()
        .get(&Key::Repository(common.to_owned()))
        .map_or(0, Weak::strong_count)
}
#[cfg(test)]
pub(super) fn record_references(store: &Store, id: &str) -> usize {
    REGISTRY
        .lock()
        .unwrap()
        .get(&Key::Record(
            store.path.canonicalize().unwrap(),
            id.to_owned(),
        ))
        .map_or(0, Weak::strong_count)
}

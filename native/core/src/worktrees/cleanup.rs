//! Retry authority for disposable checkout removal. Git can remove its admin
//! registration even when deleting the checkout fails. A pre-removal journal
//! preserves that registration; missing registration alone is never authority.
use super::{git, Record};
use crate::{paths, paths::AppPaths};
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use cap_std::fs::{Dir, MetadataExt as _, OpenOptionsExt as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt as _},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const JOURNAL_BYTES: usize = 128 * 1024 * 1024;
const ADMIN_BYTES: u64 = 16 * 1024 * 1024;
const LANE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const ENTRIES: usize = 131_072;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
}
fn identity(meta: &cap_std::fs::Metadata) -> Identity {
    Identity {
        dev: meta.dev(),
        ino: meta.ino(),
    }
}
fn directory(path: &Path) -> Result<Dir> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("Cleanup requires a real directory: {}", path.display()))?;
    ensure!(
        path.canonicalize()? == path,
        "Cleanup directory path changed"
    );
    Ok(Dir::from_std_file(file))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Entry {
    identity: Identity,
    kind: String,
    mode: u32,
    bytes: u64,
    hash: String,
    // Only Git administration is backed up, never lane/user file contents.
    data: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Tree {
    root: Identity,
    entries: BTreeMap<PathBuf, Entry>,
}
struct Budget {
    bytes: u64,
    entries: usize,
    paths: usize,
    end: Instant,
    cancel: CancellationToken,
}
impl Budget {
    fn check(&self) -> Result<()> {
        ensure!(
            !self.cancel.is_cancelled(),
            "Worktree cleanup inspection cancelled"
        );
        ensure!(
            Instant::now() < self.end,
            "Cleanup inspection exceeded 30 seconds; preserve checkout for review"
        );
        Ok(())
    }
}
fn snapshot(path: &Path, admin: bool, cancel: CancellationToken) -> Result<Tree> {
    let dir = directory(path)?;
    let mut tree = Tree {
        root: identity(&dir.dir_metadata()?),
        entries: BTreeMap::new(),
    };
    let mut budget = Budget {
        bytes: if admin { ADMIN_BYTES } else { LANE_BYTES },
        entries: 0,
        paths: 0,
        end: Instant::now() + Duration::from_secs(30),
        cancel,
    };
    #[cfg(test)]
    tests::pause_scan(path, &budget.cancel)?;
    walk(&dir, Path::new(""), admin, &mut tree.entries, &mut budget)?;
    ensure!(
        identity(&directory(path)?.dir_metadata()?) == tree.root,
        "Cleanup directory was replaced during inspection"
    );
    Ok(tree)
}
fn walk(
    dir: &Dir,
    prefix: &Path,
    admin: bool,
    out: &mut BTreeMap<PathBuf, Entry>,
    budget: &mut Budget,
) -> Result<()> {
    ensure!(
        prefix.components().count() < 128,
        "Cleanup tree is too deep; preserve for review"
    );
    for child in dir.entries()? {
        budget.check()?;
        let name = child?.file_name();
        let path = prefix.join(&name);
        budget.entries += 1;
        budget.paths += path.as_os_str().len();
        ensure!(
            budget.entries <= ENTRIES && budget.paths <= 32 * 1024 * 1024,
            "Cleanup exceeds 131,072 entries or 32 MiB of paths; preserve this checkout and review large build outputs before retrying"
        );
        let meta = dir.symlink_metadata(&name)?;
        let mut item = Entry {
            identity: identity(&meta),
            kind: String::new(),
            mode: meta.mode() & 0o7777,
            bytes: if meta.is_file() { meta.len() } else { 0 },
            hash: String::new(),
            data: None,
        };
        if meta.is_dir() {
            item.kind = "directory".into();
            // Directory permission repair is allowed, but inode replacement is not.
            item.mode = 0;
            let nested = Dir::from_std_file(
                dir.open_with(
                    &name,
                    cap_std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK),
                )?
                .into_std(),
            );
            ensure!(
                identity(&nested.dir_metadata()?) == item.identity,
                "Cleanup entry changed during inspection"
            );
            walk(&nested, &path, admin, out, budget)?;
        } else if meta.is_file() {
            item.kind = "file".into();
            let mut file = dir.open_with(
                &name,
                cap_std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK),
            )?;
            let before = file.metadata()?;
            ensure!(before.len() <= budget.bytes,
                "Cleanup exceeds its byte budget (8 GiB checkout, 16 MiB Git metadata); preserve this checkout and review large build outputs before retrying");
            ensure!(
                before.is_file()
                    && identity(&before) == item.identity
                    && (!admin || before.nlink() == 1),
                "Cleanup file identity changed or Git metadata has extra hard links"
            );
            let mut hash = Sha256::new();
            let mut saved = Vec::new();
            let mut chunk = [0u8; 65536];
            loop {
                budget.check()?;
                let n = file.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                ensure!(
                    n as u64 <= budget.bytes,
                    "Cleanup manifest exceeds its byte budget; preserve checkout for review"
                );
                budget.bytes -= n as u64;
                hash.update(&chunk[..n]);
                if admin {
                    saved.extend_from_slice(&chunk[..n]);
                }
            }
            let after = file.metadata()?;
            ensure!(
                before.len() == after.len()
                    && before.modified()? == after.modified()?
                    && before.ctime() == after.ctime()
                    && before.ctime_nsec() == after.ctime_nsec(),
                "Cleanup file changed during inspection"
            );
            item.hash = format!("{:x}", hash.finalize());
            if admin {
                item.data = Some(STANDARD.encode(saved));
            }
        } else if meta.file_type().is_symlink() && !admin {
            item.kind = "symlink".into();
            item.hash = format!(
                "{:x}",
                Sha256::digest(dir.read_link_contents(&name)?.as_os_str().as_bytes())
            );
        } else {
            anyhow::bail!(
                "Cleanup encountered a special file or Git metadata symlink; preserve for review"
            );
        }
        ensure!(
            identity(&dir.symlink_metadata(&name)?) == item.identity,
            "Cleanup entry changed during inspection"
        );
        out.insert(path, item);
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
struct Intent {
    version: u32,
    id: String,
    source: PathBuf,
    common: PathBuf,
    path: PathBuf,
    source_identity: Identity,
    common_identity: Identity,
    admin_parent_identity: Identity,
    branch: String,
    tip: String,
    delete_branch: bool,
    removed: bool,
    relocated: bool,
    quarantine: PathBuf,
    container_identity: Identity,
    lane: Tree,
    admin: Tree,
    pointer: String,
}
pub(super) struct Session {
    _lock: super::locks::Mutation,
    journal: PathBuf,
    record: Record,
    delete_branch: bool,
    intent: Option<Intent>,
}
fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn save(path: &Path, intent: &Intent) -> Result<()> {
    let bytes = serde_json::to_vec(intent)?;
    ensure!(
        bytes.len() <= JOURNAL_BYTES,
        "Cleanup journal exceeds 128 MiB; preserve checkout for review"
    );
    paths::atomic_write(path, &bytes, true)
}
fn read(path: &Path) -> Result<Option<Intent>> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        file.metadata()?.is_file(),
        "Cleanup journal must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(JOURNAL_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= JOURNAL_BYTES,
        "Cleanup journal exceeds 128 MiB"
    );
    Ok(Some(serde_json::from_slice(&bytes)?))
}
impl Session {
    pub(super) async fn open(
        paths: &AppPaths,
        record: &Record,
        delete_branch: bool,
        lock: super::locks::Mutation,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        ensure!(!cancel.is_cancelled(), "Worktree removal cancelled");
        let common = git(
            &record.source,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            cancel.clone(),
        )
        .await?;
        ensure!(
            Path::new(&common).canonicalize()? == record.common_directory,
            "Source repository identity changed"
        );
        lock.validate(&record.source, &record.common_directory)?;
        let directory = paths.data.join("managed-worktrees/records/cleanup");
        paths::private_directory(&directory)?;
        let journal = directory.join(format!("{}.json", record.id));
        let archived_journal = directory
            .parent()
            .unwrap()
            .join("archive")
            .join(format!("{}.cleanup", record.id));
        let (journal, intent) = match read(&journal)? {
            Some(intent) => (journal, Some(intent)),
            None => (archived_journal.clone(), read(&archived_journal)?),
        };
        let journal = if intent.is_none() {
            directory.join(format!("{}.json", record.id))
        } else {
            journal
        };
        ensure!(intent.is_some() || !matches!(record.state.as_str(), "removing" | "removed"),
            "Earlier cleanup has no ownership journal; preserve the checkout and registration for manual review");
        let session = Self {
            _lock: lock,
            journal,
            record: record.clone(),
            delete_branch,
            intent,
        };
        if let Some(intent) = &session.intent {
            session.identity(intent)?;
        }
        Ok(session)
    }
    fn admin_path(&self) -> PathBuf {
        self.record
            .common_directory
            .join("worktrees")
            .join(&self.record.id)
    }
    fn container(&self) -> PathBuf {
        self.record
            .path
            .parent()
            .unwrap()
            .join(".cleanup")
            .join(&self.record.id)
    }
    fn identity(&self, intent: &Intent) -> Result<()> {
        ensure!(
            intent.version == 1
                && intent.id == self.record.id
                && intent.source == self.record.source
                && intent.common == self.record.common_directory
                && intent.path == self.record.path
                && intent.branch == self.record.branch
                && intent.delete_branch == self.delete_branch
                && intent.quarantine == self.container().join("checkout"),
            "Cleanup operation identity changed; preserve recovery material"
        );
        for (path, expected) in [
            (&intent.source, &intent.source_identity),
            (&intent.common, &intent.common_identity),
        ] {
            ensure!(
                identity(&directory(path)?.dir_metadata()?) == *expected,
                "Cleanup repository directory was replaced"
            );
        }
        ensure!(
            identity(&directory(&self.container())?.dir_metadata()?) == intent.container_identity,
            "Cleanup quarantine container was replaced; preserve for review"
        );
        let parent = intent.common.join("worktrees");
        if exists(&parent)? {
            // A later Git create/prune cycle may replace the empty parent.
            // This checks only path confinement. Rebinding its identity below
            // additionally requires our exact admin ID to be absent and the
            // quarantined data/identity to remain verified.
            directory(&parent)?;
        }
        Ok(())
    }
    async fn branch_matches(&self, intent: &Intent, cancel: &CancellationToken) -> Result<()> {
        let actual = branch_tip(&self.record.source, &intent.branch, cancel).await?;
        if actual.is_none()
            && intent.delete_branch
            && intent.removed
            && !exists(&intent.quarantine)?
        {
            return Ok(());
        }
        ensure!(
            actual.as_deref() == Some(intent.tip.as_str()),
            "Managed branch changed after cleanup intent; preserve for review"
        );
        Ok(())
    }
    pub(super) async fn prepare(&mut self, tip: &str, cancel: &CancellationToken) -> Result<()> {
        if self.intent.is_some() {
            return Ok(());
        }
        let pointer = format!("gitdir: {}\n", self.admin_path().display());
        let record = self.record.clone();
        let admin = self.admin_path();
        let snapshot_cancel = cancel.clone();
        let (lane_tree, admin_tree) = tokio::task::spawn_blocking(move || -> Result<_> {
            Ok((
                snapshot(&record.path, false, snapshot_cancel.clone())?,
                snapshot(&admin, true, snapshot_cancel)?,
            ))
        })
        .await??;
        let gitfile = lane_tree
            .entries
            .get(Path::new(".git"))
            .context("Cleanup requires its original Git pointer")?;
        ensure!(
            gitfile.kind == "file"
                && gitfile.hash == format!("{:x}", Sha256::digest(pointer.as_bytes())),
            "Checkout has a foreign Git pointer"
        );
        for (name, expected) in [
            ("commondir", "../..\n".to_owned()),
            ("HEAD", format!("ref: refs/heads/{}\n", self.record.branch)),
            (
                "gitdir",
                format!("{}\n", self.record.path.join(".git").display()),
            ),
        ] {
            ensure!(
                admin_tree
                    .entries
                    .get(Path::new(name))
                    .and_then(|e| e.data.as_deref())
                    == Some(STANDARD.encode(expected).as_str()),
                "Git administration identity changed; preserve for review"
            );
        }
        ensure!(
            !admin_tree.entries.contains_key(Path::new("locked"))
                && admin_tree.entries.contains_key(Path::new("index")),
            "Cleanup requires unlocked Git administration with its original index"
        );
        let container = self.container();
        paths::private_directory(container.parent().unwrap())?;
        let mut builder = fs::DirBuilder::new();
        use std::os::unix::fs::DirBuilderExt;
        builder
            .mode(0o700)
            .create(&container)
            .context("Cleanup quarantine already exists; preserve it for review")?;
        fs::File::open(container.parent().unwrap())?.sync_all()?;
        let intent = Intent {
            version: 1,
            id: self.record.id.clone(),
            source: self.record.source.clone(),
            common: self.record.common_directory.clone(),
            path: self.record.path.clone(),
            branch: self.record.branch.clone(),
            source_identity: identity(&directory(&self.record.source)?.dir_metadata()?),
            common_identity: identity(&directory(&self.record.common_directory)?.dir_metadata()?),
            admin_parent_identity: identity(
                &directory(&self.record.common_directory.join("worktrees"))?.dir_metadata()?,
            ),
            tip: tip.into(),
            delete_branch: self.delete_branch,
            removed: false,
            relocated: false,
            quarantine: container.join("checkout"),
            container_identity: identity(&directory(&container)?.dir_metadata()?),
            lane: lane_tree,
            admin: admin_tree,
            pointer,
        };
        self.branch_matches(&intent, cancel).await?;
        save(&self.journal, &intent)?;
        self.intent = Some(intent);
        Ok(())
    }
    async fn inspect_lane(intent: &Intent, path: &Path, cancel: &CancellationToken) -> Result<()> {
        let path = path.to_owned();
        let snapshot_cancel = cancel.clone();
        let current =
            tokio::task::spawn_blocking(move || snapshot(&path, false, snapshot_cancel)).await??;
        ensure!(
            current.root == intent.lane.root,
            "Cleanup checkout was replaced; preserve for review"
        );
        ensure!(
            current
                .entries
                .iter()
                .all(|(p, e)| intent.lane.entries.get(p) == Some(e)),
            "Cleanup checkout contains new or changed data; preserve for review"
        );
        Ok(())
    }
    pub(super) async fn recover(&mut self, cancel: &CancellationToken) -> Result<()> {
        let Some(mut intent) = self.intent.clone() else {
            return Ok(());
        };
        self.identity(&intent)?;
        self.branch_matches(&intent, cancel).await?;
        let mut checked_after_move = false;
        if !intent.relocated {
            if !exists(&intent.quarantine)? {
                ensure!(
                    exists(&intent.path)?,
                    "Original checkout disappeared before quarantine; preserve recovery material"
                );
                let origin_parent = directory(intent.path.parent().unwrap())?;
                let container = directory(&self.container())?;
                ensure!(
                    identity(&container.dir_metadata()?) == intent.container_identity,
                    "Cleanup quarantine container changed"
                );
                rename_noreplace(
                    &origin_parent,
                    intent.path.file_name().unwrap(),
                    &container,
                    std::ffi::OsStr::new("checkout"),
                )?;
                origin_parent.into_std_file().sync_all()?;
                container.into_std_file().sync_all()?;
                let moved =
                    directory(&intent.quarantine).and_then(|d| Ok(identity(&d.dir_metadata()?)));
                if moved.as_ref().ok() != Some(&intent.lane.root) {
                    // A replacement raced the rename. It is never deletion
                    // authority; put it back only if the origin is still absent.
                    let restored = rename_noreplace(
                        &directory(&self.container())?,
                        std::ffi::OsStr::new("checkout"),
                        &directory(intent.path.parent().unwrap())?,
                        intent.path.file_name().unwrap(),
                    );
                    anyhow::bail!("Checkout changed during quarantine; no deletion occurred. Restoration: {restored:?}. Preserve {}", intent.quarantine.display());
                }
            }
            Self::inspect_lane(&intent, &intent.quarantine, cancel).await?;
            checked_after_move = true;
            intent.relocated = true;
            // The original pathname is no longer deletion authority, including
            // if another process recreates it before or after this durable save.
            save(&self.journal, &intent)?;
        }
        let lane_exists = exists(&intent.quarantine)?;
        if lane_exists {
            ensure!(
                !intent.removed,
                "Quarantined checkout reappeared after removal; preserve for review"
            );
            if !checked_after_move {
                Self::inspect_lane(&intent, &intent.quarantine, cancel).await?;
            }
        }
        original_replacement(&intent)?;
        let admin_path = self.admin_path();
        let planned_pointer = format!("{}\n", intent.quarantine.join(".git").display());
        if exists(&admin_path)? {
            ensure!(
                identity(&directory(admin_path.parent().unwrap())?.dir_metadata()?)
                    == intent.admin_parent_identity,
                "Cleanup administration parent changed around an occupied ID; preserve for review"
            );
            let path = admin_path.clone();
            let snapshot_cancel = cancel.clone();
            let current =
                tokio::task::spawn_blocking(move || snapshot(&path, true, snapshot_cancel))
                    .await??;
            ensure!(
                current.root == intent.admin.root && current.entries == intent.admin.entries,
                "Cleanup Git administration changed; preserve for review"
            );
            if current
                .entries
                .get(Path::new("gitdir"))
                .and_then(|e| e.data.as_deref())
                != Some(STANDARD.encode(&planned_pointer).as_str())
            {
                self.identity(&intent)?;
                rewrite_connection(&admin_path, &intent.admin, planned_pointer.as_bytes())?;
                intent.admin = snapshot(&admin_path, true, cancel.clone())?;
                // A crash before saving the new inode is ambiguous and refuses
                // on retry. Preserve the old backup and quarantined data.
                save(&self.journal, &intent)?;
            }
        } else if lane_exists {
            ensure!(!cancel.is_cancelled(), "Worktree removal cancelled");
            self.identity(&intent)?;
            let parent = admin_path.parent().unwrap();
            if !exists(parent)? {
                fs::create_dir(parent)?;
                fs::File::open(&intent.common)?.sync_all()?;
            }
            let pinned_parent = directory(parent)?;
            let parent_identity = identity(&pinned_parent.dir_metadata()?);
            // Adding only our absent ID under a verified real parent does not
            // adopt, alter, or remove unrelated worktree registrations.
            ensure!(
                !exists(&admin_path)?,
                "Cleanup admin ID became occupied; preserve for review"
            );
            if parent_identity != intent.admin_parent_identity {
                intent.admin_parent_identity = parent_identity;
                save(&self.journal, &intent)?;
            }
            let mut backup = intent.admin.clone();
            let connection = backup
                .entries
                .get_mut(Path::new("gitdir"))
                .context("Missing saved Git connection")?;
            connection.data = Some(STANDARD.encode(&planned_pointer));
            connection.hash = format!("{:x}", Sha256::digest(planned_pointer.as_bytes()));
            restore_admin(
                &admin_path,
                &backup,
                &intent.admin_parent_identity,
                &intent.common_identity,
            )?;
            intent.admin = snapshot(&admin_path, true, cancel.clone())?;
            save(&self.journal, &intent)?;
        }
        if !lane_exists && !exists(&admin_path)? && !intent.removed {
            // A crash can occur after Git deletes both owned objects but before
            // the removed receipt is saved. Durable relocation authority and
            // the unchanged branch were checked above. This records observed
            // absence, not a fabricated successful process result.
            intent.removed = true;
            save(&self.journal, &intent)?;
        }
        if lane_exists && !exists(&intent.quarantine.join(".git"))? {
            let lane = directory(&intent.quarantine)?;
            ensure!(
                identity(&lane.dir_metadata()?) == intent.lane.root,
                "Cleanup checkout was replaced"
            );
            let mut file = lane.open_with(
                ".git",
                cap_std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600),
            )?;
            file.write_all(intent.pointer.as_bytes())?;
            file.sync_all()?;
            lane.into_std_file().sync_all()?;
            let current = snapshot(&intent.quarantine, false, cancel.clone())?;
            intent.lane.entries.insert(
                PathBuf::from(".git"),
                current
                    .entries
                    .get(Path::new(".git"))
                    .context("Restored Git pointer missing")?
                    .clone(),
            );
            save(&self.journal, &intent)?;
        }
        self.intent = Some(intent);
        Ok(())
    }
    pub(super) fn removed(&mut self) -> Result<()> {
        if let Some(intent) = &mut self.intent {
            ensure!(
                !exists(&intent.quarantine)?,
                "Git reported removal but checkout remains; preserve recovery material"
            );
            intent.removed = true;
            save(&self.journal, intent)?;
        }
        Ok(())
    }
    pub(super) async fn delete_branch(&self, cancel: &CancellationToken) -> Result<bool> {
        let Some(intent) = &self.intent else {
            return Ok(false);
        };
        self.identity(intent)?;
        self.branch_matches(intent, cancel).await?;
        let reference = format!("refs/heads/{}", intent.branch);
        let registered = git(
            &intent.source,
            &["worktree", "list", "--porcelain", "-z"],
            cancel.clone(),
        )
        .await?;
        ensure!(
            !registered
                .split('\0')
                .any(|line| line == format!("branch {reference}")),
            "Managed branch is checked out elsewhere; preserve it"
        );
        // Git's compare-and-delete prevents a concurrently moved tip from
        // being silently deleted between our review and the mutation.
        if branch_tip(&intent.source, &intent.branch, cancel)
            .await?
            .is_some()
        {
            git(
                &intent.source,
                &["update-ref", "--no-deref", "-d", &reference, &intent.tip],
                cancel.clone(),
            )
            .await?;
        }
        Ok(true)
    }
    pub(super) fn archive(&self, records: &Path) -> Result<()> {
        if self.intent.is_some() {
            // Keep the bounded recovery evidence beside the removed record.
            let destination = records
                .join("archive")
                .join(format!("{}.cleanup", self.record.id));
            if self.journal == destination {
                return Ok(());
            }
            fs::rename(
                &self.journal,
                records
                    .join("archive")
                    .join(format!("{}.cleanup", self.record.id)),
            )?;
            fs::File::open(self.journal.parent().unwrap())?.sync_all()?;
            fs::File::open(records.join("archive"))?.sync_all()?;
        }
        Ok(())
    }
}
fn restore_admin(
    path: &Path,
    saved: &Tree,
    expected_parent: &Identity,
    expected_common: &Identity,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        saved.entries.len() <= ENTRIES,
        "Cleanup backup exceeds entry budget"
    );
    let mut remaining = ADMIN_BYTES;
    let common = path
        .parent()
        .and_then(Path::parent)
        .context("Missing Git admin parent")?;
    let source_parent = directory(common)?;
    ensure!(
        identity(&source_parent.dir_metadata()?) == *expected_common,
        "Cleanup common directory was replaced before restoration"
    );
    let target_parent = directory(path.parent().unwrap())?;
    ensure!(
        identity(&target_parent.dir_metadata()?) == *expected_parent,
        "Cleanup admin parent was replaced before restoration"
    );
    let staged = tempfile::Builder::new()
        .prefix(".shadowcode-cleanup-")
        .tempdir_in(format!("/proc/self/fd/{}", source_parent.as_raw_fd()))?;
    for (relative, entry) in &saved.entries {
        ensure!(
            relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "Unsafe path in cleanup backup"
        );
        let target = staged.path().join(relative);
        match entry.kind.as_str() {
            "directory" => fs::create_dir_all(&target)?,
            "file" => {
                fs::create_dir_all(target.parent().unwrap())?;
                let bytes = STANDARD.decode(
                    entry
                        .data
                        .as_ref()
                        .context("Missing cleanup backup bytes")?,
                )?;
                ensure!(
                    bytes.len() as u64 <= remaining,
                    "Cleanup backup exceeds 16 MiB"
                );
                remaining -= bytes.len() as u64;
                ensure!(
                    format!("{:x}", Sha256::digest(&bytes)) == entry.hash,
                    "Cleanup backup hash differs"
                );
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(entry.mode)
                    .open(&target)?;
                file.write_all(&bytes)?;
                file.set_permissions(fs::Permissions::from_mode(entry.mode))?;
                file.sync_all()?;
            }
            _ => anyhow::bail!("Unsafe entry in Git administration backup"),
        }
    }
    // Flush child directories before publishing the complete backup atomically.
    for (relative, entry) in saved.entries.iter().rev() {
        if entry.kind == "directory" {
            fs::File::open(staged.path().join(relative))?.sync_all()?;
        }
    }
    fs::File::open(staged.path())?.sync_all()?;
    rename_noreplace(
        &source_parent,
        staged.path().file_name().unwrap(),
        &target_parent,
        path.file_name().unwrap(),
    )?;
    target_parent.into_std_file().sync_all()?;
    Ok(())
}

fn rename_noreplace(
    from: &Dir,
    name: &std::ffi::OsStr,
    to: &Dir,
    destination: &std::ffi::OsStr,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    let from_name = std::ffi::CString::new(name.as_bytes())?;
    let to_name = std::ffi::CString::new(destination.as_bytes())?;
    let result = unsafe {
        libc::renameat2(
            from.as_raw_fd(),
            from_name.as_ptr(),
            to.as_raw_fd(),
            to_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    ensure!(
        result == 0,
        "Exclusive cleanup rename refused: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

pub(super) async fn dispose(
    paths: &AppPaths,
    source: &Path,
    id: &str,
    delete_branch: bool,
    cancel: CancellationToken,
) -> Result<Option<String>> {
    let ownership = super::locks::mutation(source, &cancel).await?;
    let (records, _) = super::roots(paths)?;
    let archived = !exists(&records.join(format!("{id}.json")))?;
    let mut record = super::read_record_identity_at(paths, id, archived)?;
    ensure!(
        record.source == crate::workspace::Workspace::open(source)?.path,
        "Worktree belongs to another source project"
    );
    let mut session = Session::open(paths, &record, delete_branch, ownership, &cancel).await?;
    if archived {
        let intent = session
            .intent
            .as_ref()
            .context("Archived cleanup has no ownership receipt; preserve for review")?;
        ensure!(
            record.state == "removed" && intent.removed && intent.relocated,
            "Archived cleanup outcome is not established"
        );
        ensure!(
            !exists(&intent.quarantine)? && !exists(&session.admin_path())?,
            "A checkout or registration reappeared after cleanup; preserve for review"
        );
        session.branch_matches(intent, &cancel).await?;
        let replacement = original_replacement(intent)?;
        if delete_branch {
            ensure!(
                branch_tip(&record.source, &record.branch, &cancel)
                    .await?
                    .is_none(),
                "Managed branch reappeared after cleanup; preserve for review"
            );
        }
        session.archive(&records)?;
        return Ok(Some(if replacement {
            "Previously completed cleanup was recovered from its durable ownership receipt; new contents at the original location were left untouched".into()
        } else {
            "Previously completed cleanup was recovered from its durable ownership receipt".into()
        }));
    }
    if session.intent.is_none() {
        ensure!(exists(&record.path)?,
            "Checkout is absent without cleanup ownership evidence; preserve its registration and index and use worktree recovery");
        let inspection = super::inspect(paths, source, id, cancel.clone()).await?;
        ensure!(
            !inspection.locked,
            "Git worktree is locked; unlock it deliberately before removal"
        );
        ensure!(
            inspection.current_branch == record.branch,
            "Checkout is no longer on its managed branch; preserve for review"
        );
        session.prepare(&inspection.head, &cancel).await?;
    }
    record.state = "removing".into();
    let quarantine = session.intent.as_ref().unwrap().quarantine.clone();
    record.detail = format!("Cleanup intent preserves original {} and quarantine {}; inspect the ownership journal if either phase needs review", record.path.display(), quarantine.display());
    super::save(&records, &record)?;
    session.recover(&cancel).await?;
    // Revalidate after any restoration/retargeting. The final Git pathname is
    // the private quarantine, never the externally visible original location.
    session.recover(&cancel).await?;
    let intent = session.intent.as_ref().unwrap();
    if !intent.removed {
        git(
            &record.source,
            &[
                "worktree",
                "remove",
                "--force",
                "--",
                quarantine.to_str().context("Cleanup path must be UTF-8")?,
            ],
            cancel.clone(),
        )
        .await
        .with_context(|| {
            format!(
                "Cleanup pending; recoverable checkout is at {}",
                quarantine.display()
            )
        })?;
        session.removed()?;
    }
    if delete_branch {
        session.delete_branch(&cancel).await?;
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
    super::save(&records, &record)?;
    let archive = records.join("archive");
    paths::private_directory(&archive)?;
    fs::rename(
        records.join(format!("{id}.json")),
        archive.join(format!("{id}.json")),
    )?;
    fs::File::open(&records)?.sync_all()?;
    fs::File::open(&archive)?.sync_all()?;
    session.archive(&records)?;
    Ok(exists(&record.path)?.then(|| {
        format!(
            "The original location {} has new contents and was left untouched",
            record.path.display()
        )
    }))
}

fn rewrite_connection(admin_path: &Path, expected: &Tree, bytes: &[u8]) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let admin = directory(admin_path)?;
    ensure!(
        identity(&admin.dir_metadata()?) == expected.root,
        "Git administration was replaced before retargeting"
    );
    let entry = expected
        .entries
        .get(Path::new("gitdir"))
        .context("Missing saved Git connection")?;
    let mut file = admin.open_with(
        "gitdir",
        cap_std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK),
    )?;
    ensure!(
        identity(&file.metadata()?) == entry.identity,
        "Git connection was replaced before retargeting"
    );
    let mut prior = Vec::new();
    (&mut file).take(16_385).read_to_end(&mut prior)?;
    ensure!(
        prior.len() <= 16_384 && format!("{:x}", Sha256::digest(&prior)) == entry.hash,
        "Git connection changed before retargeting"
    );
    // Write the opened owned inode, never truncate a replacement at its path.
    // A crash during this small write remains an explicit recovery refusal.
    file.seek(SeekFrom::Start(0))?;
    file.write_all(bytes)?;
    file.set_len(bytes.len() as u64)?;
    file.sync_all()?;
    Ok(())
}

async fn branch_tip(
    source: &Path,
    branch: &str,
    cancel: &CancellationToken,
) -> Result<Option<String>> {
    let reference = format!("refs/heads/{branch}");
    // Probe also catches dangling symbolic refs omitted by for-each-ref. A
    // symbolic alias never grants authority over its destination branch.
    ensure!(
        git(source, &["symbolic-ref", "-q", &reference], cancel.clone())
            .await
            .is_err(),
        "Managed branch became a symbolic ref; preserve it and its target for review"
    );
    let listed = git(
        source,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname) %(symref)",
            &reference,
        ],
        cancel.clone(),
    )
    .await?;
    for line in listed.lines() {
        let mut parts = line.splitn(3, ' ');
        if parts.next() != Some(reference.as_str()) {
            continue;
        }
        let tip = parts.next().context("Missing Git branch tip")?;
        ensure!(
            parts.next().is_none_or(str::is_empty),
            "Managed branch became a symbolic ref; preserve its target"
        );
        return Ok(Some(tip.to_owned()));
    }
    Ok(None)
}

fn original_replacement(intent: &Intent) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    match fs::symlink_metadata(&intent.path) {
        Ok(meta) => {
            ensure!(
                meta.dev() != intent.lane.root.dev || meta.ino() != intent.lane.root.ino,
                "Owned checkout returned to its original path; preserve for review"
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).context("Cannot inspect original cleanup location; preserve for review")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn command(path: &Path, args: &[&str]) -> String {
        let result = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Cleanup Test",
                "-c",
                "user.email=cleanup@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    }
    struct Fixture {
        _root: tempfile::TempDir,
        paths: AppPaths,
        record: Record,
        session: Session,
    }
    async fn prepared() -> Fixture {
        prepared_from(None).await
    }
    async fn prepared_from(built: Option<(&Path, &str)>) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        command(&source, &["init", "-q"]);
        fs::write(source.join("tracked.txt"), "original\n").unwrap();
        command(&source, &["add", "tracked.txt"]);
        command(&source, &["commit", "-qm", "Base"]);
        let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
        let record = super::super::create(&paths, &source, "HEAD", CancellationToken::new())
            .await
            .unwrap();
        fs::write(record.path.join("tracked.txt"), "staged lane bytes\n").unwrap();
        command(&record.path, &["add", "tracked.txt"]);
        fs::create_dir(record.path.join("blocked")).unwrap();
        fs::write(record.path.join("blocked/keep.txt"), "protected\n").unwrap();
        if let Some((built, scope)) = built {
            let started = Instant::now();
            let mut copy = std::process::Command::new("cp");
            copy.args(["-a", "--reflink=auto"]);
            let names: &[&str] = if scope == "full" {
                &[
                    "ui",
                    "native",
                    "target",
                    "Cargo.toml",
                    "Cargo.lock",
                    ".gitignore",
                    "README.md",
                    "assets",
                    "scripts",
                ]
            } else {
                &["ui", ".gitignore"]
            };
            for name in names {
                let path = built.join(name);
                assert!(
                    path.exists(),
                    "Expected built-project input {}",
                    path.display()
                );
                copy.arg(path);
            }
            assert!(copy.arg(&record.path).status().unwrap().success());
            command(&record.path, &["add", "."]);
            command(
                &record.path,
                &["commit", "-qm", "Disposable built-project fixture"],
            );
            eprintln!(
                "{}",
                serde_json::json!({"case":"built-project-cleanup","scope":scope,"copy_and_commit_seconds":started.elapsed().as_secs_f64()})
            );
        }
        let index = record
            .common_directory
            .join("worktrees")
            .join(&record.id)
            .join("index");
        fs::set_permissions(&index, fs::Permissions::from_mode(0o640)).unwrap();
        let mut session = Session::open(
            &paths,
            &record,
            true,
            super::super::locks::mutation(&record.source, &CancellationToken::new())
                .await
                .unwrap(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let started = Instant::now();
        session
            .prepare(
                &command(&record.path, &["rev-parse", "HEAD"]),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        if let Some((_, scope)) = built {
            let intent = session.intent.as_ref().unwrap();
            eprintln!(
                "{}",
                serde_json::json!({"case":"built-project-cleanup","scope":scope,"entries":intent.lane.entries.len(),"logical_bytes":intent.lane.entries.values().map(|e| e.bytes).sum::<u64>(),"journal_bytes":fs::metadata(&session.journal).unwrap().len(),"prepare_seconds":started.elapsed().as_secs_f64()})
            );
        }
        Fixture {
            _root: root,
            paths,
            record,
            session,
        }
    }
    #[tokio::test]
    async fn replaced_original_with_copied_pointer_is_never_deleted() {
        let f = prepared().await;
        let owned = f._root.path().join("preserved-owned");
        fs::rename(&f.record.path, &owned).unwrap();
        fs::create_dir(&f.record.path).unwrap();
        fs::copy(owned.join(".git"), f.record.path.join(".git")).unwrap();
        fs::write(f.record.path.join("new-user.txt"), "foreign replacement\n").unwrap();
        drop(f.session);
        let error = super::super::dispose(
            &f.paths,
            &f.record.source,
            &f.record.id,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("changed during quarantine"),
            "{error:#}"
        );
        assert_eq!(
            fs::read_to_string(f.record.path.join("new-user.txt")).unwrap(),
            "foreign replacement\n"
        );
        assert_eq!(
            fs::read_to_string(owned.join("tracked.txt")).unwrap(),
            "staged lane bytes\n"
        );
    }
    #[tokio::test]
    async fn original_path_replacement_is_outside_quarantine_deletion_authority() {
        let mut f = prepared().await;
        f.session.recover(&CancellationToken::new()).await.unwrap();
        let q = f.session.intent.as_ref().unwrap().quarantine.clone();
        fs::create_dir(&f.record.path).unwrap();
        fs::copy(q.join(".git"), f.record.path.join(".git")).unwrap();
        fs::write(
            f.record.path.join("new-user.txt"),
            "never delete this replacement\n",
        )
        .unwrap();
        drop(f.session);
        let note = super::super::dispose(
            &f.paths,
            &f.record.source,
            &f.record.id,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(note.contains("left untouched"));
        assert_eq!(
            fs::read_to_string(f.record.path.join("new-user.txt")).unwrap(),
            "never delete this replacement\n"
        );
        assert!(!q.exists());
        use std::os::unix::fs::MetadataExt;
        let replacement = fs::metadata(f.record.path.join("new-user.txt")).unwrap();
        let records = f.paths.data.join("managed-worktrees/records");
        fs::rename(
            records
                .join("archive")
                .join(format!("{}.cleanup", f.record.id)),
            records
                .join("cleanup")
                .join(format!("{}.json", f.record.id)),
        )
        .unwrap();
        for _ in 0..2 {
            assert!(super::super::dispose(
                &f.paths,
                &f.record.source,
                &f.record.id,
                CancellationToken::new()
            )
            .await
            .unwrap()
            .unwrap()
            .contains("left untouched"));
            let current = fs::metadata(f.record.path.join("new-user.txt")).unwrap();
            assert_eq!(
                (current.dev(), current.ino(), current.mode()),
                (replacement.dev(), replacement.ino(), replacement.mode())
            );
            assert_eq!(
                fs::read_to_string(f.record.path.join("new-user.txt")).unwrap(),
                "never delete this replacement\n"
            );
        }
        assert_eq!(
            fs::read_to_string(f.record.source.join("tracked.txt")).unwrap(),
            "original\n"
        );
    }
    #[tokio::test]
    async fn completed_git_removal_before_receipt_save_and_split_archive_resume() {
        let mut f = prepared().await;
        f.session.recover(&CancellationToken::new()).await.unwrap();
        let q = f.session.intent.as_ref().unwrap().quarantine.clone();
        command(
            &f.record.source,
            &["worktree", "remove", "--force", "--", q.to_str().unwrap()],
        );
        assert!(!f.session.intent.as_ref().unwrap().removed);
        assert!(!q.exists());
        drop(f.session); // Exact persisted state if process stopped before removed().
        super::super::dispose(
            &f.paths,
            &f.record.source,
            &f.record.id,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let records = f.paths.data.join("managed-worktrees/records");
        let receipt = records
            .join("archive")
            .join(format!("{}.cleanup", f.record.id));
        let pending = records
            .join("cleanup")
            .join(format!("{}.json", f.record.id));
        fs::rename(&receipt, &pending).unwrap(); // Record archived; journal not yet archived.
        assert!(super::super::dispose(
            &f.paths,
            &f.record.source,
            &f.record.id,
            CancellationToken::new()
        )
        .await
        .unwrap()
        .unwrap()
        .contains("durable ownership receipt"));
        assert!(receipt.is_file());
        assert!(!pending.exists());
        assert!(super::super::dispose(
            &f.paths,
            &f.record.source,
            &f.record.id,
            CancellationToken::new()
        )
        .await
        .is_ok());
        assert_eq!(
            command(&f.record.source, &["branch", "--list", "shadowcode/*"]),
            ""
        );
        fs::create_dir(&q).unwrap();
        fs::write(q.join("new-user.txt"), "reappeared\n").unwrap();
        assert!(super::super::dispose(
            &f.paths,
            &f.record.source,
            &f.record.id,
            CancellationToken::new()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("reappeared"));
        assert_eq!(
            fs::read_to_string(q.join("new-user.txt")).unwrap(),
            "reappeared\n"
        );
    }
    #[tokio::test]
    async fn unknown_restoration_inode_is_preserved_with_exact_staged_index() {
        let mut f = prepared().await;
        let expected_index = STANDARD
            .decode(
                f.session.intent.as_ref().unwrap().admin.entries[Path::new("index")]
                    .data
                    .as_ref()
                    .unwrap(),
            )
            .unwrap();
        f.session.recover(&CancellationToken::new()).await.unwrap();
        let q = f.session.intent.as_ref().unwrap().quarantine.clone();
        let blocked = q.join("blocked");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(
            fs::remove_file(blocked.join("keep.txt"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        let removed = git(
            &f.record.source,
            &["worktree", "remove", "--force", "--", q.to_str().unwrap()],
            CancellationToken::new(),
        )
        .await;
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(removed.is_err());
        let admin = f.session.admin_path();
        let mut pending = f.session.intent.as_ref().unwrap().clone();
        let parent = admin.parent().unwrap();
        if !exists(parent).unwrap() {
            fs::create_dir(parent).unwrap();
        }
        pending.admin_parent_identity =
            identity(&directory(parent).unwrap().dir_metadata().unwrap());
        save(&f.session.journal, &pending).unwrap();
        let journal = f.session.journal.clone();
        let before = fs::read(&journal).unwrap();
        // Invoke the actual publication primitive, stopping at the exact
        // boundary before saving its new inode or recreating q/.git.
        restore_admin(
            &admin,
            &pending.admin,
            &pending.admin_parent_identity,
            &pending.common_identity,
        )
        .unwrap();
        assert!(!q.join(".git").exists());
        assert_eq!(fs::read(admin.join("index")).unwrap(), expected_index);
        assert_eq!(
            fs::metadata(admin.join("index"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            command(
                &f.record.source,
                &["--git-dir", admin.to_str().unwrap(), "show", ":tracked.txt"]
            ),
            "staged lane bytes"
        );
        drop(f.session);
        let error = super::super::dispose(
            &f.paths,
            &f.record.source,
            &f.record.id,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("Git administration changed"),
            "{error:#}"
        );
        assert_eq!(fs::read(&journal).unwrap(), before);
        assert_eq!(fs::read(admin.join("index")).unwrap(), expected_index);
        assert_eq!(
            fs::read_to_string(blocked.join("keep.txt")).unwrap(),
            "protected\n"
        );
    }
    #[tokio::test]
    async fn missing_quarantine_with_changed_tip_or_returned_root_is_not_completion() {
        for returned in [false, true] {
            let mut f = prepared().await;
            f.session.recover(&CancellationToken::new()).await.unwrap();
            let q = f.session.intent.as_ref().unwrap().quarantine.clone();
            if returned {
                fs::rename(&q, &f.record.path).unwrap();
            } else {
                command(
                    &f.record.source,
                    &["worktree", "remove", "--force", "--", q.to_str().unwrap()],
                );
                command(
                    &f.record.source,
                    &[
                        "update-ref",
                        "-d",
                        &format!("refs/heads/{}", f.record.branch),
                    ],
                );
            }
            drop(f.session);
            let error = super::super::dispose(
                &f.paths,
                &f.record.source,
                &f.record.id,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
            assert!(
                error.to_string().contains(if returned {
                    "returned to its original"
                } else {
                    "Managed branch changed"
                }),
                "{error:#}"
            );
        }
    }
    #[tokio::test]
    async fn symbolic_managed_ref_never_authorizes_deleting_source_branch() {
        let mut f = prepared().await;
        f.session.recover(&CancellationToken::new()).await.unwrap();
        let q = f.session.intent.as_ref().unwrap().quarantine.clone();
        command(
            &f.record.source,
            &["worktree", "remove", "--force", "--", q.to_str().unwrap()],
        );
        let source_ref = command(&f.record.source, &["symbolic-ref", "HEAD"]);
        let tip = command(&f.record.source, &["rev-parse", "HEAD"]);
        let index = fs::read(f.record.common_directory.join("index")).unwrap();
        let managed = format!("refs/heads/{}", f.record.branch);
        command(&f.record.source, &["symbolic-ref", &managed, &source_ref]);
        drop(f.session);
        let error = super::super::dispose(
            &f.paths,
            &f.record.source,
            &f.record.id,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("symbolic ref"), "{error:#}");
        assert_eq!(command(&f.record.source, &["rev-parse", &source_ref]), tip);
        assert_eq!(
            command(&f.record.source, &["symbolic-ref", &managed]),
            source_ref
        );
        assert_eq!(
            fs::read(f.record.common_directory.join("index")).unwrap(),
            index
        );
        assert_eq!(
            fs::read_to_string(f.record.source.join("tracked.txt")).unwrap(),
            "original\n"
        );
        // Independently exercise the final no-deref CAS option if a symbolic
        // ref appears after inspection: only the alias may be deleted.
        command(
            &f.record.source,
            &["update-ref", "--no-deref", "-d", &managed, &tip],
        );
        assert_eq!(command(&f.record.source, &["rev-parse", &source_ref]), tip);
    }
    #[tokio::test]
    async fn readonly_origin_move_refuses_without_deletion_then_retries() {
        let f = prepared().await;
        let original = fs::read(f.record.path.join("tracked.txt")).unwrap();
        fs::set_permissions(&f.record.path, fs::Permissions::from_mode(0o555)).unwrap();
        drop(f.session);
        let result = super::super::dispose(
            &f.paths,
            &f.record.source,
            &f.record.id,
            CancellationToken::new(),
        )
        .await;
        fs::set_permissions(&f.record.path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Exclusive cleanup rename refused"));
        assert_eq!(
            fs::read(f.record.path.join("tracked.txt")).unwrap(),
            original
        );
        super::super::dispose(
            &f.paths,
            &f.record.source,
            &f.record.id,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(!f.record.path.exists());
    }
    #[tokio::test]
    #[ignore = "Opt-in disposable built-project copy; requires SHADOWCODE_CLEANUP_BUILT_FIXTURE"]
    async fn actual_built_project_cleanup_is_bounded_and_completes() {
        let source = PathBuf::from(
            std::env::var_os("SHADOWCODE_CLEANUP_BUILT_FIXTURE")
                .expect("Set explicit read-only built-project source"),
        );
        let scope = match std::env::var("SHADOWCODE_CLEANUP_BUILT_SCOPE") {
            Ok(scope) if scope == "ui" || scope == "full" => scope,
            Err(std::env::VarError::NotPresent) => "full".to_owned(),
            value => panic!("Expected explicit ui/full built-project scope, got {value:?}"),
        };
        for file in [
            "ui/package-lock.json",
            "ui/node_modules/.package-lock.json",
            "ui/dist/index.html",
        ] {
            assert!(source.join(file).is_file(), "Missing built fixture {file}");
        }
        if scope == "full" {
            assert!(source.join("target/debug").is_dir());
        }
        let f = prepared_from(Some((&source, &scope))).await;
        for file in ["ui/package-lock.json", "ui/dist/index.html"] {
            assert_eq!(
                fs::read(f.record.path.join(file)).unwrap(),
                fs::read(source.join(file)).unwrap(),
                "Disposable fixture must contain actual built source: {file}"
            );
        }
        let q = f.session.intent.as_ref().unwrap().quarantine.clone();
        let bytes: u64 = f
            .session
            .intent
            .as_ref()
            .unwrap()
            .lane
            .entries
            .values()
            .map(|e| e.bytes)
            .sum();
        if scope == "full" {
            assert!(
                bytes > 2 * 1024 * 1024 * 1024,
                "Full fixture must demonstrate the old 2 GiB refusal boundary"
            );
        } else {
            assert!(bytes > 1024 * 1024, "UI fixture must contain built outputs");
            assert!(f.session.intent.as_ref().unwrap().lane.entries.len() > 100);
        }
        drop(f.session);
        let start = Instant::now();
        super::super::dispose(
            &f.paths,
            &f.record.source,
            &f.record.id,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        eprintln!(
            "{}",
            serde_json::json!({"case":"built-project-cleanup","scope":scope,"dispose_seconds":start.elapsed().as_secs_f64(),"quarantine_removed":!q.exists(),"original_lane_removed":!f.record.path.exists()})
        );
        assert!(!q.exists() && !f.record.path.exists());
        assert_eq!(
            fs::read_to_string(f.record.source.join("tracked.txt")).unwrap(),
            "original\n"
        );
    }
    // Compiled only into this unit-test target. Production has no environment
    // variable, delay, or public API capable of holding a cleanup scan.
    static PAUSED_SCANS: std::sync::LazyLock<
        std::sync::Mutex<BTreeMap<PathBuf, std::sync::Weak<ScanPause>>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(BTreeMap::new()));
    struct PauseState {
        entered: Option<tokio::sync::oneshot::Sender<()>>,
        released: bool,
    }
    struct ScanPause {
        state: std::sync::Mutex<PauseState>,
        wake: std::sync::Condvar,
    }
    struct PausedScan {
        path: PathBuf,
        pause: std::sync::Arc<ScanPause>,
    }
    impl PausedScan {
        fn install(path: &Path) -> (Self, tokio::sync::oneshot::Receiver<()>) {
            let (send, receive) = tokio::sync::oneshot::channel();
            let pause = std::sync::Arc::new(ScanPause {
                state: std::sync::Mutex::new(PauseState {
                    entered: Some(send),
                    released: false,
                }),
                wake: std::sync::Condvar::new(),
            });
            let mut registry = PAUSED_SCANS.lock().unwrap();
            registry.retain(|_, value| value.strong_count() > 0);
            assert!(registry
                .insert(path.to_owned(), std::sync::Arc::downgrade(&pause))
                .is_none());
            (
                Self {
                    path: path.to_owned(),
                    pause,
                },
                receive,
            )
        }
        fn release(&self) {
            self.pause
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .released = true;
            self.pause.wake.notify_all();
        }
    }
    impl Drop for PausedScan {
        fn drop(&mut self) {
            self.release();
            PAUSED_SCANS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.path);
        }
    }
    pub(super) fn pause_scan(path: &Path, cancel: &CancellationToken) -> Result<()> {
        let pause = PAUSED_SCANS
            .lock()
            .unwrap()
            .remove(path)
            .and_then(|weak| weak.upgrade());
        let Some(pause) = pause else {
            return Ok(());
        };
        let mut state = pause.state.lock().unwrap();
        if let Some(entered) = state.entered.take() {
            let _ = entered.send(());
        }
        let end = Instant::now() + Duration::from_secs(10);
        while !state.released {
            ensure!(!cancel.is_cancelled(), "Test cleanup scan cancelled");
            ensure!(
                Instant::now() < end,
                "Test cleanup scan barrier was not released"
            );
            state = pause
                .wake
                .wait_timeout(state, Duration::from_millis(25))
                .unwrap()
                .0;
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unrelated_repository_create_completes_during_held_cleanup_scan() {
        let f = prepared().await;
        let q = f.session.intent.as_ref().unwrap().quarantine.clone();
        let second = f._root.path().join("independent-source");
        fs::create_dir(&second).unwrap();
        command(&second, &["init", "-q"]);
        fs::write(second.join("tracked.txt"), "independent bytes\n").unwrap();
        command(&second, &["add", "tracked.txt"]);
        command(&second, &["commit", "-qm", "Independent"]);
        let a_head = command(&f.record.source, &["rev-parse", "HEAD"]);
        let b_head = command(&second, &["rev-parse", "HEAD"]);
        let (hold, entered) = PausedScan::install(&q);
        drop(f.session);
        let paths = f.paths.clone();
        let source = f.record.source.clone();
        let id = f.record.id.clone();
        let removal = tokio::spawn(async move {
            super::super::dispose(&paths, &source, &id, CancellationToken::new()).await
        });
        tokio::time::timeout(Duration::from_secs(3), entered)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !removal.is_finished(),
            "The real scan must remain at its barrier"
        );
        let created = tokio::time::timeout(
            Duration::from_secs(2),
            super::super::create(&f.paths, &second, "HEAD", CancellationToken::new()),
        )
        .await;
        let still_held = !removal.is_finished() && !hold.pause.state.lock().unwrap().released;
        // Always release and join before asserting a regression, including the
        // old global-lock failure. No orphan task may outlive the fixture root.
        hold.release();
        tokio::time::timeout(Duration::from_secs(5), removal)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let created = created
            .expect("Unrelated repository creation waited for another repository's scan")
            .unwrap();
        assert!(
            still_held,
            "Creation only finished after the cleanup barrier released"
        );
        assert_eq!(created.state, "ready");
        assert_eq!(
            fs::read(created.path.join("tracked.txt")).unwrap(),
            b"independent bytes\n"
        );
        assert!(!f.record.path.exists() && !q.exists());
        assert_eq!(command(&f.record.source, &["rev-parse", "HEAD"]), a_head);
        assert_eq!(command(&second, &["rev-parse", "HEAD"]), b_head);
        assert_eq!(
            fs::read_to_string(f.record.source.join("tracked.txt")).unwrap(),
            "original\n"
        );
        super::super::dispose(&f.paths, &second, &created.id, CancellationToken::new())
            .await
            .unwrap();
        assert!(super::super::list(&f.paths, &second).unwrap().is_empty());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_repositories_share_one_profile_admission_limit() {
        let f = prepared().await;
        let (records, checkouts) = super::super::roots(&f.paths).unwrap();
        // Durable pre-Git reservations are valid active inventory too. Seed
        // 62 interrupted reservations plus the real existing checkout = 63.
        // This does not pretend to create 63 live Git registrations.
        for _ in 0..62 {
            let mut pending = f.record.clone();
            pending.id = crate::id();
            pending.path = checkouts.join(&pending.id);
            pending.branch = format!("shadowcode/{}", pending.id);
            pending.state = "creating".into();
            pending.detail = "Seeded durable pre-Git reservation after interrupted creation".into();
            super::super::save(&records, &pending).unwrap();
        }
        let second = f._root.path().join("second-source");
        fs::create_dir(&second).unwrap();
        command(&second, &["init", "-q"]);
        fs::write(second.join("tracked.txt"), "second\n").unwrap();
        command(&second, &["add", "tracked.txt"]);
        command(&second, &["commit", "-qm", "Second"]);
        drop(f.session);
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
        let mut tasks = Vec::new();
        for source in [f.record.source.clone(), second] {
            let paths = f.paths.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                super::super::create(&paths, &source, "HEAD", CancellationToken::new()).await
            }));
        }
        barrier.wait().await;
        let mut successes = 0;
        let mut refusals = 0;
        for task in tasks {
            match tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap()
            {
                Ok(record) => {
                    successes += 1;
                    assert_eq!(record.state, "ready");
                }
                Err(error) => {
                    refusals += 1;
                    assert!(
                        error
                            .to_string()
                            .contains("At most 64 managed worktree records"),
                        "{error:#}"
                    );
                }
            }
        }
        assert_eq!((successes, refusals), (1, 1));
        let active = fs::read_dir(&records)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .count();
        assert_eq!(active, 64);
        let interrupted = fs::read_dir(&records)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .filter(|entry| {
                serde_json::from_slice::<Record>(&fs::read(entry.path()).unwrap())
                    .unwrap()
                    .state
                    == "creating"
            })
            .count();
        assert_eq!(
            interrupted, 62,
            "Admission must not silently free interrupted ownership on restart"
        );
    }
    // Separate follow-up regression: expected to fail even after the core lock
    // patch until worktree_tasks::LOCK is replaced. No model is called; the real
    // task prepare/discard entrypoints and actual cleanup scan are exercised.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unrelated_worktree_task_prepare_completes_during_held_task_cleanup() {
        use crate::{engine::Engine, store::keys, worktree_tasks};
        let f = prepared().await;
        let q = f.session.intent.as_ref().unwrap().quarantine.clone();
        let second = f._root.path().join("task-source-b");
        fs::create_dir(&second).unwrap();
        command(&second, &["init", "-q"]);
        fs::write(second.join("tracked.txt"), "independent task bytes\n").unwrap();
        command(&second, &["add", "tracked.txt"]);
        command(&second, &["commit", "-qm", "Task B"]);
        let before = command(&second, &["rev-parse", "HEAD"]);
        drop(f.session);
        let engine = Engine::open(f.paths.clone()).unwrap();
        let store = engine.store();
        let session = store
            .create_session(&f.record.path, "fixture-only", "Task A")
            .unwrap();
        let task_id = crate::id();
        let task: worktree_tasks::Record = serde_json::from_value(serde_json::json!({
            "id":task_id,"workspace":f.record.source,"session_id":session["id"],
            "worktree":f.record.path,"worktree_id":f.record.id,"branch":f.record.branch,
            "base":{"commit":f.record.base_commit,"head":f.record.base_commit},
            "state":"done","status":"completed","task":"Recorded local fixture"
        }))
        .unwrap();
        store
            .set_native_meta(
                &keys::worktree_task_record(&task_id),
                &serde_json::to_string(&task).unwrap(),
            )
            .unwrap();
        store
            .set_native_meta(
                &keys::worktree_task_index(&task.workspace),
                &serde_json::json!([task_id]).to_string(),
            )
            .unwrap();
        let (hold, entered) = PausedScan::install(&q);
        let closing_engine = engine.clone();
        let closing_id = task_id.clone();
        let closing =
            tokio::spawn(
                async move { worktree_tasks::discard(&closing_engine, &closing_id).await },
            );
        tokio::time::timeout(Duration::from_secs(3), entered)
            .await
            .unwrap()
            .unwrap();
        let created = tokio::time::timeout(
            Duration::from_secs(2),
            worktree_tasks::prepare(
                &engine,
                &second,
                "Task B without a model turn",
                &[],
                "fixture-only",
            ),
        )
        .await;
        let visibility = if let Ok(Ok(record)) = &created {
            Some(
                tokio::time::timeout(Duration::from_secs(2), async {
                    let got = worktree_tasks::get(&engine, &record.id).await?;
                    let listed = worktree_tasks::list(&engine, &second).await?;
                    Ok::<_, anyhow::Error>((got, listed))
                })
                .await,
            )
        } else {
            None
        };
        let still_held = !closing.is_finished() && !hold.pause.state.lock().unwrap().released;
        hold.release();
        let closed = tokio::time::timeout(Duration::from_secs(5), closing)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let created_ok = created
            .as_ref()
            .ok()
            .and_then(|result| result.as_ref().ok())
            .cloned();
        if let Some(record) = created_ok {
            worktree_tasks::abandon(&engine, record).await;
        }
        engine.shutdown().await.unwrap();
        assert!(
            closed.removed,
            "Task A cleanup did not complete: {closed:?}"
        );
        let created = created
            .expect("Task B was blocked by worktree_tasks global LOCK while Task A scanned")
            .unwrap();
        assert!(
            still_held,
            "Task B only prepared after Task A released its scan"
        );
        assert_eq!(created.state, "starting");
        let (got, listed) = visibility
            .expect("Task B must exist for visibility checks")
            .expect("Unrelated task get/list waited for Task A's scan")
            .unwrap();
        assert_eq!(got.id, created.id);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);
        assert_eq!(command(&second, &["rev-parse", "HEAD"]), before);
        assert_eq!(
            fs::read(second.join("tracked.txt")).unwrap(),
            b"independent task bytes\n"
        );
        assert!(!q.exists());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_task_discard_and_cancelled_waiter_close_the_same_record_once() {
        use crate::{engine::Engine, store::keys, worktree_tasks};
        let f = prepared().await;
        let q = f.session.intent.as_ref().unwrap().quarantine.clone();
        let source_index = fs::read(f.record.source.join(".git/index")).unwrap();
        let source_head = command(&f.record.source, &["rev-parse", "HEAD"]);
        drop(f.session);
        let engine = Engine::open(f.paths.clone()).unwrap();
        let store = engine.store();
        let session = store
            .create_session(&f.record.path, "fixture-only", "One close")
            .unwrap();
        let task_id = crate::id();
        let task: worktree_tasks::Record = serde_json::from_value(serde_json::json!({
            "id":task_id,"workspace":f.record.source,"session_id":session["id"],
            "worktree":f.record.path,"worktree_id":f.record.id,"branch":f.record.branch,
            "base":{"commit":f.record.base_commit,"head":f.record.base_commit},
            "state":"done","status":"completed","task":"Single close fixture"
        }))
        .unwrap();
        store
            .set_native_meta(
                &keys::worktree_task_record(&task_id),
                &serde_json::to_string(&task).unwrap(),
            )
            .unwrap();
        store
            .set_native_meta(
                &keys::worktree_task_index(&task.workspace),
                &serde_json::json!([task_id]).to_string(),
            )
            .unwrap();
        let (hold, entered) = PausedScan::install(&q);
        let owner_engine = engine.clone();
        let owner_id = task_id.clone();
        let owner =
            tokio::spawn(async move { worktree_tasks::discard(&owner_engine, &owner_id).await });
        tokio::time::timeout(Duration::from_secs(3), entered)
            .await
            .unwrap()
            .unwrap();
        let cancelled_engine = engine.clone();
        let cancelled_id = task_id.clone();
        let cancelled =
            tokio::spawn(
                async move { worktree_tasks::discard(&cancelled_engine, &cancelled_id).await },
            );
        let queued = tokio::time::timeout(Duration::from_secs(3), async {
            while worktree_tasks::record_lock_references(&store, &task_id) < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        cancelled.abort();
        let cancelled_result = tokio::time::timeout(Duration::from_secs(1), cancelled).await;
        let second_engine = engine.clone();
        let second_id = task_id.clone();
        let second =
            tokio::spawn(async move { worktree_tasks::discard(&second_engine, &second_id).await });
        let second_queued = tokio::time::timeout(Duration::from_secs(3), async {
            while worktree_tasks::record_lock_references(&store, &task_id) < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        let still_held = !owner.is_finished()
            && !second.is_finished()
            && !hold.pause.state.lock().unwrap().released;
        // All actual work is joined before checking regressions; the temporary
        // repository never disappears beneath a detached cleanup task.
        hold.release();
        let first = tokio::time::timeout(Duration::from_secs(5), owner)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let repeated = tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        engine.shutdown().await.unwrap();
        assert!(
            queued.is_ok() && second_queued.is_ok(),
            "Both real discard calls must reach the shared queue"
        );
        assert!(cancelled_result.unwrap().unwrap_err().is_cancelled());
        assert!(still_held);
        assert!(first.removed && first.state == "discarded");
        assert_eq!(first.to_json(), repeated.to_json(), "Second close must acknowledge the same outcome without duplicate notes or state changes");
        assert_eq!(
            worktree_tasks::load(&store, &task_id).unwrap().to_json(),
            first.to_json()
        );
        let managed = super::super::task_record(&f.paths, &f.record.id).unwrap();
        assert_eq!(managed.state, "removed");
        let records = f.paths.data.join("managed-worktrees/records");
        assert!(!records.join(format!("{}.json", f.record.id)).exists());
        let mut archived: Vec<_> = fs::read_dir(records.join("archive"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        archived.sort();
        assert_eq!(
            archived,
            vec![
                format!("{}.cleanup", f.record.id),
                format!("{}.json", f.record.id)
            ]
        );
        assert!(!q.exists() && !f.record.path.exists());
        assert_eq!(
            fs::read(f.record.source.join(".git/index")).unwrap(),
            source_index
        );
        assert_eq!(
            command(&f.record.source, &["rev-parse", "HEAD"]),
            source_head
        );
        assert!(command(&f.record.source, &["branch", "--list", &f.record.branch]).is_empty());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_cleanup_blocks_new_commands_and_background_processes_during_original_scan() {
        use crate::{
            config::Config,
            engine::{CommandRequest, Engine, StartRequest},
            store::keys,
            worktree_tasks,
        };
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        command(&source, &["init", "-q"]);
        fs::write(source.join("tracked.txt"), "original\n").unwrap();
        command(&source, &["add", "."]);
        command(&source, &["commit", "-qm", "Base"]);
        let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
        Config::patch(&paths, serde_json::json!({"cli_agents":{"enabled":false}, "permissions":{"mode":"ask","approve_shell":true},
            "model":{"provider":"local","name":"fixture-only","default":"fixture-only","endpoint":"http://127.0.0.1:9/v1","context_limit":16384}})).unwrap();
        let engine = Engine::open(paths.clone()).unwrap();
        let mut task =
            worktree_tasks::prepare(&engine, &source, "No model turn", &[], "fixture-only")
                .await
                .unwrap();
        task.state = "done".into();
        task.status = "completed".into();
        engine
            .store()
            .set_native_meta(
                &keys::worktree_task_record(&task.id),
                &serde_json::to_string(&task).unwrap(),
            )
            .unwrap();
        let config = Config::load(&paths, Some(&task.worktree)).unwrap();
        let (hold, entered) = PausedScan::install(&task.worktree);
        let closing_engine = engine.clone();
        let closing_id = task.id.clone();
        let closing =
            tokio::spawn(
                async move { worktree_tasks::discard(&closing_engine, &closing_id).await },
            );
        tokio::time::timeout(Duration::from_secs(5), entered)
            .await
            .unwrap()
            .unwrap();
        assert!(
            task.worktree.is_dir(),
            "Barrier must hold original checkout before rename"
        );
        let command_result = engine
            .start_command(
                StartRequest {
                    workspace: task.worktree.clone(),
                    task: "Never run".into(),
                    session_id: Some(task.session_id.clone()),
                    model: None,
                    mode: "command".into(),
                    queue: true,
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
        let background_result = engine.background().start(
            &task.worktree,
            &config,
            None,
            "blocked-fixture",
            "sleep 300",
        );
        hold.release();
        let closed = tokio::time::timeout(Duration::from_secs(5), closing)
            .await
            .unwrap()
            .unwrap();
        engine.shutdown().await.unwrap();
        assert!(command_result
            .unwrap_err()
            .to_string()
            .contains("manual operation"));
        assert!(background_result
            .unwrap_err()
            .to_string()
            .contains("being removed"));
        assert!(closed.unwrap().removed);
        assert!(!task.worktree.exists() && !source.join("must-not-run.txt").exists());
        assert_eq!(
            fs::read_to_string(source.join("tracked.txt")).unwrap(),
            "original\n"
        );
    }
}

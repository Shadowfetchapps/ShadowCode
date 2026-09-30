//! Your data: backups, restore, repair and reset (Settings › Your data,
//! `/api/data…`, `shadowcode backup|restore|reset|doctor --repair`).
//!
//! A **backup** is a folder `shadowcode-backup-<local time>/` holding a
//! consistent copy of the profile database (`VACUUM INTO`, taken while the
//! app runs), `config.yaml`, your agent definitions
//! (`~/.config/shadowcode/agents`) and plugin install records, plus
//! `manifest.json` with each file's size and SHA-256. API keys
//! (`secrets.env`, plus the keys moved to the desktop keyring) and
//! remote-access pairing (`remote.json`) are copied only when the user asks
//! for them.
//!
//! **Restore** and **reset** replace files the running engine has open, so
//! they are scheduled: the request validates everything, stages the files in
//! the state folder and writes a small marker; [`apply_pending`] finishes the
//! job the next time an engine opens the profile, after it holds the profile
//! lock and before it opens the database. A restore first backs up what it
//! replaces; a reset moves folders aside and deletes nothing. API keys and
//! remote-access pairing come back only when asked for, each on its own;
//! remote access comes back switched off, and its devices and phone
//! notifications wait until the user turns it on.
//!
//! **Repair** runs in place: it backs up the database first, checks its
//! integrity, rebuilds indexes and statistics, and moves regenerable cache
//! files into that backup folder (so even that is reversible).
use crate::{
    paths::{atomic_write, private_directory, AppPaths},
    store::{self, open_immutable, Store},
};
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

pub const BACKUP_FORMAT: &str = "shadowcode-backup";
pub const BACKUP_FORMAT_VERSION: u32 = 1;
pub const MANIFEST: &str = "manifest.json";
/// State-folder marker of a scheduled restore or reset.
const PENDING: &str = "pending-data-operation.json";
/// State-folder record of the last finished restore, reset or repair.
const LAST: &str = "last-data-operation.json";
/// State-folder copy of the files a scheduled restore will put in place.
const STAGING: &str = "pending-restore";
const DATABASE: &str = "state/shadow-agent.db";
/// API keys: in a backup and restored only when asked for.
const KEYS: &str = "config/secrets.env";
/// Remote access and paired devices: backed up with the API keys, restored
/// only when asked for on its own.
const REMOTE: &str = "config/remote.json";
/// Folders in the data directory a reset leaves where they are: the backups
/// themselves, Git worktrees holding project changes (Git keeps pointing at
/// them) and large downloads the user would otherwise fetch again.
pub const KEPT_ON_RESET: &[&str] = &[
    "backups",
    "managed-worktrees",
    "parallel-worktrees",
    crate::local_downloads::DIR,
    "voice",
    "code-intel",
];
/// Regenerable caches that Repair moves into its backup folder.
const CACHE_FILES: &[&str] = &["state/openrouter-models.json"];
/// Largest single non-database file copied into a backup.
const MAX_SMALL_FILE: u64 = 4 * 1024 * 1024;
/// Most small files (agents, plugin records) copied into one backup. More
/// are left out and listed, never a reason to fail: the backups made before
/// a restore or a repair must not be blocked by an unusual agents folder.
const MAX_SMALL_FILES: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    /// `config/…`, `state/…`, `data/…` or `agents/…` (your agent
    /// definitions), always with `/`.
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub format: String,
    pub format_version: u32,
    /// The ShadowCode version that made the backup.
    pub app_version: String,
    /// The database format (`PRAGMA user_version`) of the copied database.
    pub schema_version: i64,
    pub created_at: f64,
    /// The backup holds at least one API key.
    pub includes_secrets: bool,
    /// `manual`, `before-restore`, `before-repair` or `upgrade-copy`.
    pub reason: String,
    pub files: Vec<FileEntry>,
    /// The database could not be read consistently (a damaged database
    /// backed up before a restore), so its files were copied as they were.
    #[serde(default)]
    pub raw_copy: bool,
    /// What the backup could not include, in plain words: a key the keyring
    /// did not give, files over the size or count limits.
    #[serde(default)]
    pub left_out: Vec<String>,
}

/// Where backups go unless the user picks another folder.
pub fn backups_dir(paths: &AppPaths) -> PathBuf {
    paths.data.join("backups")
}

fn sha256_file(path: &Path) -> Result<(u64, String)> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    let mut bytes = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        hasher.update(&buffer[..count]);
    }
    Ok((bytes, format!("{:x}", hasher.finalize())))
}

#[cfg(unix)]
fn private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
#[cfg(not(unix))]
fn private_file(_path: &Path) -> Result<()> {
    Ok(())
}

/// `~/x` to the home folder; other paths unchanged.
fn expand(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.to_path_buf(),
    }
}

/// Make a copied file private and durable before anything relies on it.
fn seal(path: &Path) -> Result<()> {
    private_file(path)?;
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn sync_dir(path: &Path) {
    if let Ok(dir) = fs::File::open(path) {
        let _ = dir.sync_all();
    }
}

/// `config/x`, `state/x`, `data/x` or `agents/x` to a real path, refusing
/// anything that could leave those folders.
fn resolve(paths: &AppPaths, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    ensure!(
        !relative.is_empty()
            && !relative.contains('\\')
            && path.components().all(|c| matches!(c, Component::Normal(_))),
        "Backup file path is not allowed: {relative}"
    );
    let mut components = path.components();
    let root = match components.next() {
        Some(Component::Normal(first)) if first == "config" => paths.config.clone(),
        Some(Component::Normal(first)) if first == "state" => paths.state.clone(),
        Some(Component::Normal(first)) if first == "data" => paths.data.clone(),
        // Your own agent definitions (`~/.config/shadowcode/agents`).
        Some(Component::Normal(first)) if first == "agents" => crate::agents::user_dir(paths),
        _ => bail!("Backup file path is not allowed: {relative}"),
    };
    let rest = components.as_path();
    ensure!(
        !rest.as_os_str().is_empty(),
        "Backup file path is not allowed: {relative}"
    );
    Ok(root.join(rest))
}

/// Files a restore puts back. Anything else in a (newer) backup is listed
/// as not restored.
fn restorable(relative: &str) -> bool {
    matches!(relative, DATABASE | "config/config.yaml")
        || secret_file(relative)
        || relative.starts_with("agents/")
        || relative.starts_with("state/native-plugins/")
}

/// Files copied into a backup only when the user asks for API keys.
fn secret_file(relative: &str) -> bool {
    matches!(relative, KEYS | REMOTE)
}

/// "1 file", "3 files".
fn count(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// The small files a backup copies as they are, and what it left out.
#[derive(Default)]
struct Collected {
    /// `(source, relative)`.
    files: Vec<(PathBuf, String)>,
    left_out: Vec<String>,
}

impl Collected {
    /// Regular files below `dir`, as `root/…`. Symlinks and hidden files and
    /// folders (such as `.git`) are skipped; files over the size or count
    /// limits are noted in `left_out` instead of failing the backup.
    fn add_tree(&mut self, dir: &Path, root: &str) -> Result<()> {
        let (mut large, mut over) = (Vec::new(), 0);
        self.walk(dir, root, &mut large, &mut over)?;
        if let Some(first) = large.first() {
            self.left_out.push(format!(
                "{root}: {} larger than 4 MB, such as {first}",
                count(large.len(), "file")
            ));
        }
        if over > 0 {
            self.left_out.push(format!(
                "{root}: {} past the limit of {MAX_SMALL_FILES} files",
                count(over, "file")
            ));
        }
        Ok(())
    }

    fn walk(
        &mut self,
        dir: &Path,
        prefix: &str,
        large: &mut Vec<String>,
        over: &mut usize,
    ) -> Result<()> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(());
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let kind = entry.file_type()?;
            let relative = format!("{prefix}/{name}");
            if kind.is_dir() {
                self.walk(&entry.path(), &relative, large, over)?;
            } else if !kind.is_file() {
                continue;
            } else if entry.metadata()?.len() > MAX_SMALL_FILE {
                large.push(relative);
            } else if self.files.len() >= MAX_SMALL_FILES {
                *over += 1;
            } else {
                self.files.push((entry.path(), relative));
            }
        }
        Ok(())
    }
}

/// What a backup holds besides the database, settings, agents and plugin
/// records.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Extras {
    None,
    /// The user asked for API keys: `secrets.env` written by [`backup_keys`]
    /// (with the keys moved to the keyring) and `remote.json`.
    Secrets,
    /// Before a restore: the files it replaces, copied as they are. The
    /// keyring is never read: a restore leaves the keys in it where they are.
    Replaced {
        keys: bool,
        remote: bool,
    },
}

/// The profile files a backup copies as they are.
fn profile_files(paths: &AppPaths, extras: Extras) -> Result<Collected> {
    let (keys, remote) = match extras {
        Extras::None => (false, false),
        Extras::Secrets => (false, true),
        Extras::Replaced { keys, remote } => (keys, remote),
    };
    let mut collected = Collected::default();
    for (relative, wanted) in [("config/config.yaml", true), (KEYS, keys), (REMOTE, remote)] {
        if !wanted {
            continue;
        }
        let source = resolve(paths, relative)?;
        if fs::symlink_metadata(&source).is_ok_and(|m| m.is_file()) {
            collected.files.push((source, relative.to_owned()));
        }
    }
    collected.add_tree(&crate::agents::user_dir(paths), "agents")?;
    collected.add_tree(&paths.state.join("native-plugins"), "state/native-plugins")?;
    Ok(collected)
}

/// The text of a backup's `secrets.env`: the keys saved in the file plus
/// the keys moved to the keyring (read from it now), so a restore on a new
/// computer or after a reset brings every key back. Also returns how many
/// keys it holds and the keys the keyring did not give.
fn backup_keys(paths: &AppPaths) -> Result<(String, usize, Vec<String>)> {
    let mut keys = crate::config::secrets(paths)?;
    let mut left_out = Vec::new();
    for name in crate::keyring::listed(paths) {
        match crate::keyring::get(paths, &name) {
            Ok(Some(value)) => {
                keys.insert(name, value);
            }
            Ok(None) => left_out.push(format!("{name}: not found in the keyring")),
            Err(error) => left_out.push(format!("{name}: the keyring did not give it ({error:#})")),
        }
    }
    let mut text = String::new();
    for (name, value) in &keys {
        text.push_str(&format!("{name}={}\n", serde_json::to_string(value)?));
    }
    Ok((text, keys.len(), left_out))
}

/// A folder inside a backup folder this process just created with mode 700,
/// where nobody else can have put anything.
fn backup_subfolder(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .with_context(|| format!("Cannot create {}", path.display()))
}

/// A new backup folder, held open while it is written. The folder the user
/// chose may be one other accounts can change: every write goes through the
/// open folder (`/proc/self/fd/…`), so it lands in the folder this process
/// made even if someone renames it or puts a symlink in its place.
struct NewFolder {
    path: PathBuf,
    #[cfg(unix)]
    handle: fs::File,
}

impl NewFolder {
    /// Open the folder just made and make sure it is that folder: a real,
    /// empty directory, owned by this account when other accounts can
    /// change `parent` (elsewhere a network share may show another owner).
    fn open(path: PathBuf, parent: &Path) -> Result<Self> {
        let replaced = format!(
            "The new backup folder {} was replaced while it was being made; choose a folder only you can change",
            path.display()
        );
        #[cfg(unix)]
        let folder = {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            let handle = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)
                .with_context(|| replaced.clone())?;
            // Group or others may write, and no sticky bit stops them from
            // renaming what this account made.
            let shared =
                fs::metadata(parent).is_ok_and(|m| m.mode() & 0o022 != 0 && m.mode() & 0o1000 == 0);
            ensure!(
                !shared || handle.metadata()?.uid() == unsafe { libc::geteuid() },
                "{replaced}"
            );
            Self { path, handle }
        };
        #[cfg(not(unix))]
        let folder = {
            let _ = parent;
            Self { path }
        };
        ensure!(
            fs::read_dir(folder.inside())?.next().is_none(),
            "{replaced}"
        );
        Ok(folder)
    }

    /// Where to write: the open folder, whatever its name leads to now.
    fn inside(&self) -> PathBuf {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let held = PathBuf::from(format!("/proc/self/fd/{}", self.handle.as_raw_fd()));
            if held.is_dir() {
                return held;
            }
        }
        self.path.clone()
    }

    /// Its name still leads to the folder this process made.
    fn in_place(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let (Ok(named), Ok(held)) = (fs::symlink_metadata(&self.path), self.handle.metadata())
            else {
                return false;
            };
            named.dev() == held.dev() && named.ino() == held.ino()
        }
        #[cfg(not(unix))]
        true
    }

    /// Never leave a half-written backup that looks usable.
    fn discard(&self) {
        let inside = self.inside();
        for entry in fs::read_dir(&inside).into_iter().flatten().flatten() {
            let _ = if entry.file_type().is_ok_and(|t| t.is_dir()) {
                fs::remove_dir_all(entry.path())
            } else {
                fs::remove_file(entry.path())
            };
        }
        if self.in_place() {
            let _ = fs::remove_dir(&self.path);
        }
    }
}

/// A new, private (mode 700), uniquely named folder inside `parent`.
fn new_backup_folder(parent: &Path, reason: &str) -> Result<NewFolder> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let suffix = if reason == "manual" {
        String::new()
    } else {
        format!("-{reason}")
    };
    for attempt in 1..1000 {
        let name = if attempt == 1 {
            format!("shadowcode-backup-{stamp}{suffix}")
        } else {
            format!("shadowcode-backup-{stamp}{suffix}-{attempt}")
        };
        let folder = parent.join(name);
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&folder) {
            Ok(()) => return NewFolder::open(folder, parent),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Cannot create a backup in {}", parent.display()))
            }
        }
    }
    bail!(
        "Cannot find a free backup folder name in {}",
        parent.display()
    )
}

/// A consistent copy of the live database, taken through its own read
/// connection (`VACUUM INTO` reads one snapshot; writers keep working).
fn copy_database(source: &Path, target: &Path) -> Result<i64> {
    // Read-write without CREATE: a database in WAL mode may need its
    // `-shm` file created to be read; VACUUM INTO never writes the source.
    let connection = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("Cannot open the database to back it up")?;
    connection.busy_timeout(std::time::Duration::from_secs(10))?;
    let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    connection
        .execute("VACUUM INTO ?", [target.to_string_lossy()])
        .context("Cannot copy the database")?;
    drop(connection);
    // VACUUM INTO keeps the header's user_version; make sure of it.
    let copy = Connection::open(target)?;
    copy.pragma_update(None, "user_version", version)?;
    drop(copy);
    seal(target)?;
    Ok(version)
}

#[derive(Clone, Copy, Debug)]
pub struct BackupOptions<'a> {
    pub include_secrets: bool,
    /// Parent folder; `None` uses [`backups_dir`].
    pub folder: Option<&'a Path>,
    pub reason: &'a str,
    /// Copy a database that cannot be read consistently as it is, instead
    /// of failing (used before a restore replaces a damaged database).
    pub allow_raw: bool,
}

/// Create a backup and return its manifest and folder.
pub fn create_backup(paths: &AppPaths, options: BackupOptions<'_>) -> Result<(PathBuf, Manifest)> {
    let extras = if options.include_secrets {
        Extras::Secrets
    } else {
        Extras::None
    };
    backup(paths, options, extras)
}

fn backup(
    paths: &AppPaths,
    options: BackupOptions<'_>,
    extras: Extras,
) -> Result<(PathBuf, Manifest)> {
    let parent = match options.folder {
        Some(folder) => {
            ensure!(
                folder.is_absolute(),
                "Choose a full folder path for backups"
            );
            // The user's own folder (Documents, a shared folder, a USB drive,
            // maybe through a symlink) stays as it is; only a missing one is
            // created. The backup folder inside it is private.
            fs::create_dir_all(folder)
                .with_context(|| format!("Cannot create the folder {}", folder.display()))?;
            folder.to_path_buf()
        }
        None => {
            let folder = backups_dir(paths);
            private_directory(&folder)?;
            folder
        }
    };
    let folder = new_backup_folder(&parent, options.reason)?;
    let written = write_backup(paths, &folder.inside(), options, extras).and_then(|manifest| {
        ensure!(
            folder.in_place(),
            "The backup folder {} was moved or replaced while it was being written; choose a folder only you can change",
            folder.path.display()
        );
        Ok(manifest)
    });
    match written {
        Ok(manifest) => Ok((folder.path, manifest)),
        Err(error) => {
            folder.discard();
            Err(error)
        }
    }
}

fn write_backup(
    paths: &AppPaths,
    folder: &Path,
    options: BackupOptions<'_>,
    extras: Extras,
) -> Result<Manifest> {
    let mut files = Vec::new();
    let mut raw_copy = false;
    let mut schema_version = 0;
    let database = paths.database();
    if database.is_file() {
        let target = folder.join(DATABASE);
        backup_subfolder(target.parent().context("backup folder")?)?;
        match copy_database(&database, &target) {
            Ok(version) => schema_version = version,
            Err(error) if options.allow_raw => {
                tracing::warn!("database copied as it is: {error:#}");
                let _ = fs::remove_file(&target);
                raw_copy = true;
                for suffix in ["", "-wal", "-shm"] {
                    let source = PathBuf::from(format!("{}{suffix}", database.display()));
                    if source.is_file() {
                        let relative = format!("{DATABASE}{suffix}");
                        let copy = folder.join(&relative);
                        fs::copy(&source, &copy)?;
                        seal(&copy)?;
                        if suffix.is_empty() {
                            continue; // added below with the others
                        }
                        let (bytes, sha256) = sha256_file(&copy)?;
                        files.push(FileEntry {
                            path: relative,
                            bytes,
                            sha256,
                        });
                    }
                }
            }
            Err(error) => return Err(error),
        }
        let (bytes, sha256) = sha256_file(&target)?;
        files.insert(
            0,
            FileEntry {
                path: DATABASE.into(),
                bytes,
                sha256,
            },
        );
    }
    let Collected {
        files: copies,
        mut left_out,
    } = profile_files(paths, extras)?;
    for (source, relative) in copies {
        let target = folder.join(&relative);
        backup_subfolder(target.parent().context("backup folder")?)?;
        fs::copy(&source, &target)
            .with_context(|| format!("Cannot copy {} into the backup", source.display()))?;
        seal(&target)?;
        let (bytes, sha256) = sha256_file(&target)?;
        files.push(FileEntry {
            path: relative,
            bytes,
            sha256,
        });
    }
    // A copied `secrets.env` holds keys unless it is empty (every key moved
    // to the keyring).
    let mut includes_secrets = files.iter().any(|f| f.path == KEYS)
        && crate::config::secrets(paths).is_ok_and(|keys| !keys.is_empty());
    if extras == Extras::Secrets {
        let (text, keys, missing) = backup_keys(paths)?;
        left_out.extend(missing);
        if keys > 0 {
            let target = folder.join(KEYS);
            backup_subfolder(target.parent().context("backup folder")?)?;
            atomic_write(&target, text.as_bytes(), true)?;
            let (bytes, sha256) = sha256_file(&target)?;
            files.push(FileEntry {
                path: KEYS.into(),
                bytes,
                sha256,
            });
            includes_secrets = true;
        }
    }
    let manifest = Manifest {
        format: BACKUP_FORMAT.into(),
        format_version: BACKUP_FORMAT_VERSION,
        app_version: crate::VERSION.into(),
        schema_version,
        created_at: crate::now(),
        includes_secrets,
        reason: options.reason.into(),
        files,
        raw_copy,
        left_out,
    };
    for directory in ["state", "config", "agents"] {
        sync_dir(&folder.join(directory));
    }
    atomic_write(
        &folder.join(MANIFEST),
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
        true,
    )?;
    sync_dir(folder);
    Ok(manifest)
}

fn is_sqlite(path: &Path) -> bool {
    let mut header = [0u8; 16];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .is_ok()
        && &header == b"SQLite format 3\0"
}

/// What a database copy holds, for the restore preview.
fn database_summary(path: &Path) -> Result<(i64, Value)> {
    let db = open_immutable(path)?;
    let check: String = db
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .context("The database copy cannot be read")?;
    ensure!(
        check == "ok",
        "The database copy is damaged ({check}); choose another backup"
    );
    let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    ensure!(
        ["sessions", "tasks", "events"]
            .iter()
            .all(|t| tables.iter().any(|name| name == t)),
        "This is not a ShadowCode database"
    );
    let count = |sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let has = |table: &str| tables.iter().any(|name| name == table);
    let summary = json!({
        "conversations": count("SELECT count(*) FROM sessions"),
        "tasks": count("SELECT count(*) FROM tasks"),
        "jobs": if has("desktop_jobs") { count("SELECT count(*) FROM desktop_jobs") } else { 0 },
        "goals": if has("goals") { count("SELECT count(*) FROM goals") } else { 0 },
        "automations": if has("automations") { count("SELECT count(*) FROM automations") } else { 0 },
        "comparisons": if has("native_meta") { count("SELECT count(*) FROM native_meta WHERE key LIKE 'compare:%'") } else { 0 },
        "last_activity": db.query_row("SELECT max(updated_at) FROM sessions", [], |r| r.get::<_, Option<f64>>(0)).unwrap_or(None),
    });
    Ok((version, summary))
}

/// The result of checking a backup without changing anything.
#[derive(Clone, Debug, Serialize)]
pub struct Inspection {
    /// The backup folder, or the database file for an automatic copy.
    pub path: PathBuf,
    /// `backup` (a folder with a manifest) or `database` (a bare copy such
    /// as the automatic copy made before an upgrade).
    pub kind: String,
    pub manifest: Manifest,
    pub summary: Value,
    /// Files that a restore will not put back (unknown to this version).
    pub ignored: Vec<String>,
    /// Why this backup cannot be restored; empty when it can.
    pub problems: Vec<String>,
    pub restorable: bool,
}

/// Check a backup folder (or its `manifest.json`, or a bare database copy):
/// format, every file's size and digest, database integrity and version,
/// and that its settings load. Nothing is written.
pub fn inspect(paths: &AppPaths, path: &Path) -> Result<Inspection> {
    let path = expand(path);
    ensure!(path.is_absolute(), "Give the full path of a backup folder");
    let metadata =
        fs::metadata(&path).with_context(|| format!("No backup found at {}", path.display()))?;
    if metadata.is_file() && path.file_name().is_some_and(|n| n == MANIFEST) {
        return inspect(paths, path.parent().context("backup folder")?);
    }
    if metadata.is_file() {
        ensure!(
            is_sqlite(&path),
            "{} is neither a backup folder nor a ShadowCode database",
            path.display()
        );
        let (bytes, sha256) = sha256_file(&path)?;
        let mut problems = Vec::new();
        let (version, summary) = match database_summary(&path) {
            Ok(found) => found,
            Err(error) => {
                problems.push(format!("{error:#}"));
                (0, Value::Null)
            }
        };
        if version > store::SCHEMA_VERSION {
            problems.push(newer_database(version));
        }
        let created_at = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64());
        return Ok(Inspection {
            path,
            kind: "database".into(),
            restorable: problems.is_empty(),
            problems,
            summary,
            ignored: Vec::new(),
            manifest: Manifest {
                format: BACKUP_FORMAT.into(),
                format_version: BACKUP_FORMAT_VERSION,
                app_version: String::new(),
                schema_version: version,
                created_at,
                includes_secrets: false,
                reason: "upgrade-copy".into(),
                files: vec![FileEntry {
                    path: DATABASE.into(),
                    bytes,
                    sha256,
                }],
                raw_copy: false,
                left_out: Vec::new(),
            },
        });
    }
    ensure!(
        metadata.is_dir(),
        "{} is not a backup folder",
        path.display()
    );
    let manifest_path = path.join(MANIFEST);
    let text = fs::read_to_string(&manifest_path).with_context(|| {
        format!(
            "{} has no {MANIFEST}; it is not a ShadowCode backup (or it is incomplete)",
            path.display()
        )
    })?;
    let manifest: Manifest = serde_json::from_str(&text)
        .with_context(|| format!("{MANIFEST} in {} is damaged", path.display()))?;
    ensure!(
        manifest.format == BACKUP_FORMAT,
        "{} is not a ShadowCode backup",
        path.display()
    );
    let mut problems = Vec::new();
    if manifest.format_version > BACKUP_FORMAT_VERSION {
        problems.push(format!(
            "This backup was made by a newer ShadowCode ({}); update ShadowCode to restore it",
            manifest.app_version
        ));
    }
    let mut ignored = Vec::new();
    let mut summary = Value::Null;
    let mut has_database = false;
    for file in &manifest.files {
        if resolve(paths, &file.path).is_err() {
            problems.push(format!(
                "The backup lists a file outside the profile: {}",
                file.path
            ));
            continue;
        }
        let source = path.join(&file.path);
        match fs::symlink_metadata(&source) {
            Ok(m) if m.is_file() => {}
            Ok(_) => {
                problems.push(format!("{} in the backup is not a regular file", file.path));
                continue;
            }
            Err(_) => {
                problems.push(format!(
                    "The backup is incomplete: {} is missing",
                    file.path
                ));
                continue;
            }
        }
        let (bytes, sha256) = sha256_file(&source)?;
        if bytes != file.bytes || sha256 != file.sha256 {
            problems.push(format!(
                "{} changed or is damaged since the backup was made",
                file.path
            ));
            continue;
        }
        if !restorable(&file.path) {
            ignored.push(file.path.clone());
            continue;
        }
        if file.path == DATABASE {
            has_database = true;
            if manifest.raw_copy {
                problems.push(
                    "This backup holds a damaged database copied as it was; it cannot be restored"
                        .into(),
                );
                continue;
            }
            match database_summary(&source) {
                Ok((version, found)) => {
                    summary = found;
                    if version > store::SCHEMA_VERSION {
                        problems.push(newer_database(version));
                    }
                }
                Err(error) => problems.push(format!("{error:#}")),
            }
        }
        if file.path == "config/config.yaml" {
            if let Err(error) = check_settings(&source) {
                problems.push(format!(
                    "The backup's settings (config.yaml) do not load in this version: {error:#}"
                ));
            }
        }
    }
    if !has_database {
        problems.push("The backup has no database".into());
    }
    Ok(Inspection {
        path,
        kind: "backup".into(),
        restorable: problems.is_empty(),
        problems,
        summary,
        ignored,
        manifest,
    })
}

fn newer_database(version: i64) -> String {
    format!(
        "This database was written by a newer ShadowCode (database format {version}; this version reads up to {}). Update ShadowCode to restore it",
        store::SCHEMA_VERSION
    )
}

/// Load a `config.yaml` in a throwaway profile.
fn check_settings(file: &Path) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let paths = AppPaths::isolated(temp.path())?;
    fs::copy(file, paths.config_file())?;
    crate::config::Config::load(&paths, None)?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    /// `restore` or `reset`.
    pub kind: String,
    pub requested_at: f64,
    /// Restore: the backup it came from, and whether API keys and remote
    /// access come back too.
    #[serde(default)]
    pub source: Option<PathBuf>,
    #[serde(default)]
    pub include_secrets: bool,
    #[serde(default)]
    pub include_remote: bool,
    /// Restore: the staged files (paths relative to the staging folder).
    #[serde(default)]
    pub manifest: Option<Manifest>,
}

/// What a restore brings back besides the database, settings, agents and
/// plugin records.
#[derive(Clone, Copy, Debug, Default)]
pub struct RestoreOptions {
    /// API keys (`secrets.env`), when the backup has them.
    pub include_secrets: bool,
    /// Remote access, paired devices and phone notification settings
    /// (`remote.json`), when the backup has them. Devices removed since the
    /// backup come back too, so remote access comes back switched off and
    /// the restored devices and notifications wait until the user turns it
    /// on after checking them.
    pub include_remote: bool,
}

pub fn pending(paths: &AppPaths) -> Result<Option<Pending>> {
    match fs::read(paths.state.join(PENDING)) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("The scheduled data operation is unreadable")?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn last_operation(paths: &AppPaths) -> Option<Value> {
    serde_json::from_slice(&fs::read(paths.state.join(LAST)).ok()?).ok()
}

fn record_last(paths: &AppPaths, value: Value) {
    if let Err(error) = atomic_write(
        &paths.state.join(LAST),
        serde_json::to_string_pretty(&value)
            .unwrap_or_default()
            .as_bytes(),
        true,
    ) {
        tracing::warn!("could not record the data operation: {error:#}");
    }
}

/// Cancel a scheduled restore or reset.
pub fn cancel_pending(paths: &AppPaths) -> Result<bool> {
    let marker = paths.state.join(PENDING);
    let existed = marker.exists();
    let _ = fs::remove_dir_all(paths.state.join(STAGING));
    if existed {
        fs::remove_file(&marker).context("Could not cancel the scheduled data operation")?;
    }
    Ok(existed)
}

fn write_pending(paths: &AppPaths, pending: &Pending) -> Result<()> {
    atomic_write(
        &paths.state.join(PENDING),
        serde_json::to_string_pretty(pending)?.as_bytes(),
        true,
    )
}

/// Validate a backup and stage it for the next start.
pub fn schedule_restore(paths: &AppPaths, path: &Path, options: RestoreOptions) -> Result<Pending> {
    let inspection = inspect(paths, path)?;
    ensure!(
        inspection.restorable,
        "This backup cannot be restored: {}",
        inspection.problems.join("; ")
    );
    cancel_pending(paths)?;
    let staging = paths.state.join(STAGING);
    private_directory(&staging)?;
    let mut files = Vec::new();
    for file in &inspection.manifest.files {
        let wanted = match file.path.as_str() {
            KEYS => options.include_secrets,
            REMOTE => options.include_remote,
            _ => true,
        };
        if !restorable(&file.path) || !wanted {
            continue;
        }
        let source = if inspection.kind == "database" {
            inspection.path.clone()
        } else {
            inspection.path.join(&file.path)
        };
        let target = staging.join(&file.path);
        private_directory(target.parent().context("staging folder")?)?;
        fs::copy(&source, &target)
            .with_context(|| format!("Cannot copy {} from the backup", file.path))?;
        seal(&target)?;
        // The staged copy must be exactly what was checked.
        let (bytes, sha256) = sha256_file(&target)?;
        ensure!(
            bytes == file.bytes && sha256 == file.sha256,
            "{} changed while it was being copied; try again",
            file.path
        );
        files.push(if file.path == REMOTE {
            switched_off(&target)?
        } else {
            file.clone()
        });
    }
    let mut manifest = inspection.manifest.clone();
    manifest.files = files;
    let has = |relative: &str| manifest.files.iter().any(|f| f.path == relative);
    manifest.includes_secrets = has(KEYS);
    let include_remote = has(REMOTE);
    let pending = Pending {
        kind: "restore".into(),
        requested_at: crate::now(),
        source: Some(inspection.path),
        include_secrets: manifest.includes_secrets,
        include_remote,
        manifest: Some(manifest),
    };
    write_pending(paths, &pending)?;
    Ok(pending)
}

/// Turn remote access off in a staged `remote.json` and hold what came back
/// with it: its paired devices may include ones removed since the backup,
/// and its phone notification topic one a lost phone still follows. The
/// devices cannot connect (not even through `shadowcode serve --remote`,
/// which ignores the switch) and nothing is sent until the user turns
/// remote access on in Settings › Remote access, having checked them
/// (`remote::Settings::confirm_restored`). Returns the staged file's new
/// entry.
fn switched_off(staged: &Path) -> Result<FileEntry> {
    let mut settings: Value = serde_json::from_slice(&fs::read(staged)?)
        .ok()
        .filter(Value::is_object)
        .context("The backup's remote-access settings (remote.json) cannot be read")?;
    settings["enabled"] = Value::Bool(false);
    for device in settings["devices"].as_array_mut().into_iter().flatten() {
        if device.is_object() {
            device["restored"] = Value::Bool(true);
        }
    }
    let set = |key: &str| {
        settings["ntfy"][key]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    };
    if set("server") && set("topic") {
        settings["ntfy"]["restored"] = Value::Bool(true);
    }
    atomic_write(staged, &serde_json::to_vec_pretty(&settings)?, true)?;
    let (bytes, sha256) = sha256_file(staged)?;
    Ok(FileEntry {
        path: REMOTE.into(),
        bytes,
        sha256,
    })
}

/// Schedule moving everything aside at the next start.
pub fn schedule_reset(paths: &AppPaths) -> Result<Pending> {
    cancel_pending(paths)?;
    let pending = Pending {
        kind: "reset".into(),
        requested_at: crate::now(),
        source: None,
        include_secrets: false,
        include_remote: false,
        manifest: None,
    };
    write_pending(paths, &pending)?;
    Ok(pending)
}

/// Finish a scheduled restore or reset. Call with the profile lock held and
/// before the database is opened. A failure is recorded for Settings › Your
/// data and returned; the marker is removed either way so a broken backup
/// cannot stop every later start.
pub fn apply_pending(paths: &AppPaths) -> Result<Option<Value>> {
    let pending = match pending(paths) {
        Ok(Some(pending)) => pending,
        Ok(None) => return Ok(None),
        Err(error) => {
            let _ = fs::remove_file(paths.state.join(PENDING));
            record_last(
                paths,
                json!({"kind": "restore", "ok": false, "finished_at": crate::now(), "error": format!("{error:#}")}),
            );
            return Err(error);
        }
    };
    let result = match pending.kind.as_str() {
        "restore" => apply_restore(paths, &pending),
        "reset" => apply_reset(paths, &pending),
        other => Err(anyhow::anyhow!("Unknown scheduled data operation: {other}")),
    };
    let _ = fs::remove_file(paths.state.join(PENDING));
    let _ = fs::remove_dir_all(paths.state.join(STAGING));
    match result {
        Ok(value) => {
            record_last(paths, value.clone());
            Ok(Some(value))
        }
        Err(error) => {
            record_last(
                paths,
                json!({"kind": pending.kind, "ok": false, "finished_at": crate::now(), "source": pending.source, "error": format!("{error:#}")}),
            );
            Err(error)
        }
    }
}

fn apply_restore(paths: &AppPaths, pending: &Pending) -> Result<Value> {
    let manifest = pending
        .manifest
        .as_ref()
        .context("The scheduled restore lists no files")?;
    let staging = paths.state.join(STAGING);
    for file in &manifest.files {
        ensure!(restorable(&file.path), "Unexpected file {}", file.path);
        resolve(paths, &file.path)?;
        let (bytes, sha256) = sha256_file(&staging.join(&file.path))
            .with_context(|| format!("The staged {} is missing", file.path))?;
        ensure!(
            bytes == file.bytes && sha256 == file.sha256,
            "The staged {} changed; nothing was restored",
            file.path
        );
    }
    // Keep what is about to be replaced.
    let (before, _) = backup(
        paths,
        BackupOptions {
            include_secrets: false,
            folder: None,
            reason: "before-restore",
            allow_raw: true,
        },
        Extras::Replaced {
            keys: pending.include_secrets,
            remote: pending.include_remote,
        },
    )
    .context("Could not back up the current data before restoring; nothing was restored")?;
    // Prepare every replacement next to its target, then swap them in.
    let mut prepared = Vec::new();
    for file in &manifest.files {
        let target = resolve(paths, &file.path)?;
        let parent = target.parent().context("profile folder")?;
        private_directory(parent)?;
        let temporary = parent.join(format!(
            ".{}.restoring",
            target.file_name().context("file name")?.to_string_lossy()
        ));
        fs::copy(staging.join(&file.path), &temporary)?;
        private_file(&temporary)?;
        fs::File::open(&temporary)?.sync_all()?;
        prepared.push((temporary, target, file.path.clone()));
    }
    let database = paths.database();
    let mut replaced = Vec::new();
    for (temporary, target, relative) in &prepared {
        if relative == DATABASE {
            // The old write-ahead log belongs to the old database; applying it
            // to the restored file would corrupt it. It is in the backup.
            for suffix in ["-wal", "-shm", "-journal"] {
                let _ = fs::remove_file(format!("{}{suffix}", database.display()));
            }
        }
        if let Err(error) = fs::rename(temporary, target) {
            for (_, target, relative) in &replaced {
                // A file that did not exist before the restore (no
                // secrets.env yet, for example) is removed again.
                let previous = before.join(relative);
                let _ = if previous.is_file() {
                    fs::copy(previous, target).map(|_| ())
                } else {
                    fs::remove_file(target)
                };
            }
            for (temporary, _, _) in &prepared {
                let _ = fs::remove_file(temporary);
            }
            return Err(anyhow::Error::from(error).context(format!(
                "Could not put {relative} in place; the previous data is in {}",
                before.display()
            )));
        }
        sync_dir(target.parent().unwrap_or(Path::new("/")));
        replaced.push((temporary.clone(), target.clone(), relative.clone()));
    }
    if pending.include_secrets {
        // The restored keys are the ones asked for: read them from
        // secrets.env, not from the keyring they may have been moved to.
        let restored = crate::config::file_secret_names(paths).unwrap_or_default();
        if let Err(error) = crate::keyring::prefer_file(paths, &restored) {
            tracing::warn!("restore could not update keyring.json: {error:#}");
        }
    }
    Ok(json!({
        "kind": "restore",
        "ok": true,
        "finished_at": crate::now(),
        "source": pending.source,
        "restored": manifest.files.iter().map(|f| &f.path).collect::<Vec<_>>(),
        "secrets_restored": pending.include_secrets,
        "remote_restored": pending.include_remote,
        "backup_of_previous_data": before,
        "from_version": manifest.app_version,
    }))
}

fn apply_reset(paths: &AppPaths, _pending: &Pending) -> Result<Value> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut asides = Vec::new();
    let roots = [&paths.config, &paths.data, &paths.state];
    let result = (|| -> Result<()> {
        // XDG_CONFIG_HOME, XDG_DATA_HOME and XDG_STATE_HOME may name the same
        // folder, or one inside another: each folder is handled once, keeping
        // what any of its roles keeps, and never moving another role's
        // folder with it.
        let mut done: Vec<&PathBuf> = Vec::new();
        for root in roots {
            if done.contains(&root) {
                continue;
            }
            done.push(root);
            let parent = root.parent().context("profile folder has no parent")?;
            let name = root.file_name().context("profile folder name")?;
            let mut aside = parent.join(format!("{}.reset-{stamp}", name.to_string_lossy()));
            let mut attempt = 2;
            while aside.exists() {
                aside = parent.join(format!(
                    "{}.reset-{stamp}-{attempt}",
                    name.to_string_lossy()
                ));
                attempt += 1;
            }
            let mut entries: Vec<_> = fs::read_dir(root)?.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let file = entry.file_name().to_string_lossy().into_owned();
                let path = entry.path();
                // native.lock stays: moving it would let a second engine
                // lock a fresh one while this one runs.
                let keep = (root == &paths.state
                    && matches!(file.as_str(), "native.lock" | PENDING))
                    || (root == &paths.data && KEPT_ON_RESET.contains(&file.as_str()))
                    || roots.iter().any(|other| other.starts_with(&path))
                    || asides.contains(&path);
                if keep {
                    continue;
                }
                if !aside.exists() {
                    private_directory(&aside)?;
                    asides.push(aside.clone());
                }
                let target = aside.join(entry.file_name());
                fs::rename(entry.path(), &target)
                    .with_context(|| format!("Could not move {} aside", entry.path().display()))?;
                moved.push((entry.path(), target));
            }
            sync_dir(root);
            sync_dir(parent);
        }
        Ok(())
    })();
    if let Err(error) = result {
        // Put back what was moved, newest first.
        for (original, target) in moved.iter().rev() {
            let _ = fs::rename(target, original);
        }
        for aside in &asides {
            let _ = fs::remove_dir(aside);
        }
        return Err(error.context("Reset was undone; nothing was moved"));
    }
    Ok(json!({
        "kind": "reset",
        "ok": true,
        "finished_at": crate::now(),
        "moved_to": asides,
        "moved": moved.len(),
        "kept": KEPT_ON_RESET,
    }))
}

/// A backup folder listed in Settings › Your data.
#[derive(Clone, Debug, Serialize)]
pub struct Listed {
    pub path: PathBuf,
    pub name: String,
    pub app_version: String,
    pub schema_version: i64,
    pub created_at: f64,
    pub includes_secrets: bool,
    pub reason: String,
    pub bytes: u64,
}

/// Backups in `folder` (newest first), read from their manifests; the
/// digests are checked only when one is inspected or restored.
pub fn list_backups(folder: &Path) -> Vec<Listed> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(folder) else {
        return found;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path().join(MANIFEST)) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<Manifest>(&text) else {
            continue;
        };
        if manifest.format != BACKUP_FORMAT {
            continue;
        }
        found.push(Listed {
            path: entry.path(),
            name: entry.file_name().to_string_lossy().into_owned(),
            app_version: manifest.app_version,
            schema_version: manifest.schema_version,
            created_at: manifest.created_at,
            includes_secrets: manifest.includes_secrets,
            reason: manifest.reason,
            bytes: manifest.files.iter().map(|f| f.bytes).sum(),
        });
    }
    found.sort_by(|a, b| b.created_at.total_cmp(&a.created_at));
    found
}

/// The database copies made automatically before each upgrade
/// (`shadow-agent.pre-native-<id>.sqlite` next to the database).
pub fn upgrade_copies(paths: &AppPaths) -> Vec<Value> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(&paths.state) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("shadow-agent.pre-native-") && name.ends_with(".sqlite")) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64());
        let version = open_immutable(&entry.path())
            .and_then(|db| Ok(db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?))
            .ok();
        found.push(json!({"path": entry.path(), "name": name, "bytes": metadata.len(), "created_at": modified, "schema_version": version}));
    }
    found.sort_by(|a, b| {
        b["created_at"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["created_at"].as_f64().unwrap_or(0.0))
    });
    found
}

/// Folders an earlier reset moved aside (siblings named `<folder>.reset-…`).
pub fn reset_folders(paths: &AppPaths) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in [&paths.config, &paths.data, &paths.state] {
        let (Some(parent), Some(name)) = (root.parent(), root.file_name()) else {
            continue;
        };
        let prefix = format!("{}.reset-", name.to_string_lossy());
        for entry in fs::read_dir(parent).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix)
                && entry.file_type().is_ok_and(|t| t.is_dir())
            {
                found.push(entry.path());
            }
        }
    }
    // Two folders are the same when the XDG variables name one place.
    found.sort();
    found.dedup();
    found
}

/// One line of a repair report.
fn check(id: &str, label: &str, status: &str, detail: impl Into<String>) -> Value {
    json!({"id": id, "label": label, "status": status, "detail": detail.into()})
}

/// Check and repair the profile database in place. A backup is made first;
/// damage is reported (restore a backup to fix it), never "fixed" by
/// dropping data.
pub fn repair(paths: &AppPaths, store: &Store) -> Result<Value> {
    let (backup, _) = create_backup(
        paths,
        BackupOptions {
            include_secrets: false,
            folder: None,
            reason: "before-repair",
            allow_raw: true,
        },
    )
    .context("Could not back up the database before repairing it; nothing was changed")?;
    let mut checks = Vec::new();
    let db = store.lock()?;
    let integrity: Vec<String> = db
        .prepare("PRAGMA integrity_check(20)")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()
        .unwrap_or_else(|error| vec![error.to_string()]);
    let healthy = integrity == ["ok"];
    checks.push(if healthy {
        check("integrity", "Database integrity", "pass", "No damage found")
    } else {
        check(
            "integrity",
            "Database integrity",
            "fail",
            format!(
                "Damage found: {}. Restore a backup from Settings › Your data (a copy of the current database is in {}).",
                integrity.join("; "),
                backup.display()
            ),
        )
    });
    let violations: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    checks.push(if violations == 0 {
        check("references", "Links between records", "pass", "All records point to existing ones")
    } else {
        check(
            "references",
            "Links between records",
            "warn",
            format!("{violations} records point to ones that no longer exist; they are kept as they are"),
        )
    });
    if healthy {
        match db.execute_batch("REINDEX; ANALYZE; PRAGMA optimize;") {
            Ok(()) => checks.push(check("indexes", "Search indexes", "pass", "Rebuilt")),
            Err(error) => checks.push(check(
                "indexes",
                "Search indexes",
                "fail",
                error.to_string(),
            )),
        }
    } else {
        checks.push(check(
            "indexes",
            "Search indexes",
            "not_checked",
            "Not rebuilt because the database is damaged",
        ));
    }
    match db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(0) => checks.push(check(
            "journal",
            "Write-ahead log",
            "pass",
            "Merged into the database",
        )),
        Ok(_) => checks.push(check(
            "journal",
            "Write-ahead log",
            "warn",
            "Another ShadowCode process is reading; the log will be merged later",
        )),
        Err(error) => checks.push(check(
            "journal",
            "Write-ahead log",
            "fail",
            error.to_string(),
        )),
    }
    drop(db);
    let mut cleared = Vec::new();
    for relative in CACHE_FILES {
        let source = resolve(paths, relative)?;
        if !source.is_file() {
            continue;
        }
        let target = backup.join("caches").join(relative);
        private_directory(target.parent().context("backup folder")?)?;
        if fs::rename(&source, &target).is_ok()
            || fs::copy(&source, &target)
                .and_then(|_| fs::remove_file(&source))
                .is_ok()
        {
            cleared.push(relative.to_string());
        }
    }
    let leftovers = [paths.state.join(STAGING)]
        .into_iter()
        .filter(|p| p.exists() && !paths.state.join(PENDING).exists())
        .collect::<Vec<_>>();
    for leftover in &leftovers {
        let _ = fs::rename(leftover, backup.join("leftover-staging"));
    }
    checks.push(check(
        "caches",
        "Caches",
        "pass",
        if cleared.is_empty() {
            "No cache files to clear".to_owned()
        } else {
            format!(
                "Cleared {} (moved into the repair backup)",
                cleared.join(", ")
            )
        },
    ));
    let value = json!({
        "kind": "repair",
        "ok": healthy,
        "finished_at": crate::now(),
        "checks": checks,
        "cleared": cleared,
        "backup": backup,
    });
    record_last(paths, value.clone());
    Ok(value)
}

/// Settings › Your data: folders, database, backups and scheduled work.
pub fn overview(paths: &AppPaths) -> Result<Value> {
    let database = paths.database();
    let size = |path: PathBuf| fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let schema_version = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|db| db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)))
    .ok();
    Ok(json!({
        "folders": {"config": paths.config, "data": paths.data, "state": paths.state},
        "database": {
            "path": database,
            "bytes": size(database.clone()),
            "wal_bytes": size(PathBuf::from(format!("{}-wal", database.display()))),
            "schema_version": schema_version,
            "supported_schema_version": store::SCHEMA_VERSION,
        },
        "app_version": crate::VERSION,
        "backups_folder": backups_dir(paths),
        "backups": list_backups(&backups_dir(paths)),
        "upgrade_copies": upgrade_copies(paths),
        "reset_folders": reset_folders(paths),
        "kept_on_reset": KEPT_ON_RESET,
        "pending": pending(paths)?.map(|p| json!({"kind": p.kind, "requested_at": p.requested_at, "source": p.source, "include_secrets": p.include_secrets, "include_remote": p.include_remote})),
        "last_operation": last_operation(paths),
    }))
}

/// What to close so a scheduled restore or reset runs. It runs when an
/// engine next opens the profile, so the process that holds it now must
/// stop: `mode` is that engine's (`control::Server`), `None` when unknown.
/// A desktop window attached to another engine does not hold the profile.
pub fn restart_hint(mode: Option<&str>, pid: u32) -> String {
    match mode {
        Some("desktop") => "Quit ShadowCode and open it again.".into(),
        Some("server") => format!(
            "`shadowcode serve` (process {pid}) holds your data: stop it, then open ShadowCode again."
        ),
        Some("acp") => format!(
            "An editor runs ShadowCode's agent (`shadowcode acp`, process {pid}) and holds your data: close ShadowCode in that editor, or quit the editor, then open ShadowCode again."
        ),
        Some("tui") => format!(
            "ShadowCode is open in a terminal (process {pid}) and holds your data: quit it there, then open ShadowCode again."
        ),
        Some("command") => format!(
            "A `shadowcode` command (process {pid}) holds your data: wait for it to finish, then open ShadowCode again."
        ),
        _ => "Quit ShadowCode and anything else using your data (`shadowcode serve`, ShadowCode in a terminal, or an editor running `shadowcode acp`), then open ShadowCode again.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Someone who can change the chosen folder renames the new backup
    /// folder and puts a symlink to their own folder in its place: every
    /// file still lands in the folder this process made, the backup is
    /// refused, and their folder is left alone.
    #[cfg(unix)]
    #[test]
    fn a_backup_folder_swapped_for_a_symlink_keeps_its_files_private() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::isolated(&temp.path().join("profile")).unwrap();
        crate::config::set_secret(&paths, "OPENAI_API_KEY", "private-value").unwrap();
        let shared = temp.path().join("shared");
        let theirs = temp.path().join("theirs");
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(&theirs).unwrap();
        let folder = new_backup_folder(&shared, "manual").unwrap();
        let moved = shared.join("moved");
        fs::rename(&folder.path, &moved).unwrap();
        std::os::unix::fs::symlink(&theirs, &folder.path).unwrap();
        let options = BackupOptions {
            include_secrets: true,
            folder: Some(&shared),
            reason: "manual",
            allow_raw: false,
        };
        write_backup(&paths, &folder.inside(), options, Extras::Secrets).unwrap();
        assert_eq!(fs::read_dir(&theirs).unwrap().count(), 0);
        assert!(moved.join(MANIFEST).is_file());
        assert!(fs::read_to_string(moved.join(KEYS))
            .unwrap()
            .contains("private-value"));
        assert!(!folder.in_place(), "the backup is refused");
        folder.discard();
        assert_eq!(fs::read_dir(&moved).unwrap().count(), 0);
        assert!(fs::symlink_metadata(&folder.path)
            .unwrap()
            .file_type()
            .is_symlink());

        // Only the new, empty, real folder is used.
        let link = shared.join("link");
        std::os::unix::fs::symlink(&theirs, &link).unwrap();
        assert!(NewFolder::open(link, &shared).is_err());
        fs::write(theirs.join("planted"), "x").unwrap();
        assert!(NewFolder::open(theirs.clone(), temp.path()).is_err());
    }
}

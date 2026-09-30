use crate::{id, now};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, types::ValueRef, Connection, OptionalExtension, Params};
use serde_json::{json, Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

mod automations;
mod background;
mod editor_drafts;
mod goals;
pub mod keys;
mod memory;
mod meta;
pub use automations::AutomationRun;
pub use goals::MilestoneSpec;
pub use meta::MetaTransaction;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
 id TEXT PRIMARY KEY, workspace TEXT NOT NULL, created_at REAL NOT NULL,
 updated_at REAL NOT NULL, model_id TEXT, status TEXT NOT NULL,
 title TEXT, usage_json TEXT, parent_id TEXT, branched_at REAL
);
CREATE TABLE IF NOT EXISTS tasks (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
 prompt TEXT NOT NULL, status TEXT NOT NULL, summary TEXT,
 created_at REAL NOT NULL, completed_at REAL, usage_json TEXT
);
CREATE TABLE IF NOT EXISTS events (
 id INTEGER PRIMARY KEY AUTOINCREMENT, ts REAL NOT NULL, type TEXT NOT NULL,
 session_id TEXT, task_id TEXT, payload TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_session_id ON events(session_id,id);
CREATE INDEX IF NOT EXISTS events_task_id ON events(task_id,id);
CREATE TABLE IF NOT EXISTS models (
 id TEXT PRIMARY KEY, name TEXT NOT NULL, provider TEXT NOT NULL,
 endpoint TEXT, context_limit INTEGER, metadata TEXT
);
CREATE TABLE IF NOT EXISTS projects (
 id TEXT PRIMARY KEY, path TEXT UNIQUE NOT NULL, name TEXT NOT NULL, last_opened REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS session_meta (
 session_id TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(session_id,key)
);
CREATE TABLE IF NOT EXISTS pins (
 id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
 task_id TEXT, ts REAL NOT NULL, label TEXT NOT NULL, body TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS desktop_jobs (id TEXT PRIMARY KEY, payload TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS background_processes (
 id TEXT PRIMARY KEY, workspace TEXT NOT NULL, started_at REAL NOT NULL,
 status TEXT NOT NULL, payload TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS background_workspace ON background_processes(workspace,started_at);
CREATE TABLE IF NOT EXISTS native_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS goals (
 id TEXT PRIMARY KEY, workspace TEXT NOT NULL, instruction TEXT NOT NULL,
 status TEXT NOT NULL, progress REAL NOT NULL, title TEXT,
 created_at REAL NOT NULL, updated_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS milestones (
 id TEXT PRIMARY KEY, goal_id TEXT NOT NULL REFERENCES goals(id),
 title TEXT NOT NULL, status TEXT NOT NULL, order_index INTEGER NOT NULL,
 detail TEXT, task_id TEXT, created_at REAL NOT NULL, updated_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS goal_runs (
 goal_id TEXT PRIMARY KEY REFERENCES goals(id), session_id TEXT NOT NULL,
 job_id TEXT, status TEXT NOT NULL, detail TEXT NOT NULL, updated_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS file_changes (
 id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL,
 workspace TEXT NOT NULL, path TEXT NOT NULL, before_bytes BLOB,
 before_mode INTEGER, after_hash TEXT, observed_hash TEXT, restored INTEGER NOT NULL DEFAULT 0,
 UNIQUE(task_id,workspace,path)
);
CREATE TABLE IF NOT EXISTS job_messages (
 job_id TEXT NOT NULL, ordinal INTEGER NOT NULL, payload TEXT NOT NULL,
 PRIMARY KEY(job_id,ordinal)
);
CREATE TABLE IF NOT EXISTS queued_tasks (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL, workspace TEXT NOT NULL,
 payload TEXT NOT NULL, status TEXT NOT NULL, created_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS task_notes (
 task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
 content TEXT NOT NULL, updated_at REAL NOT NULL
);
"#;

/// Current schema version. Older databases are backed up, then migrated
/// forward one step at a time inside one transaction.
pub const SCHEMA_VERSION: i64 = 27;

/// Version 27: bounded, profile-private editor drafts. Draft bytes are never
/// written into the project until the user explicitly saves a reviewed file.
const MIGRATION_27: &str = r#"
CREATE TABLE IF NOT EXISTS editor_drafts (
 workspace TEXT NOT NULL, path TEXT NOT NULL, revision TEXT NOT NULL,
 base_hash TEXT NOT NULL, base TEXT NOT NULL, draft TEXT NOT NULL,
 updated_at REAL NOT NULL, PRIMARY KEY(workspace,path)
);
CREATE INDEX IF NOT EXISTS editor_drafts_workspace ON editor_drafts(workspace,updated_at);
"#;

/// Version 25: persisted subscription usage snapshots, so the Accounts page
/// and picker can show "Last checked …" before the first refresh. Execution
/// targets and vendor session ids live in `session_meta` / `native_meta`.
const MIGRATION_25: &str = r#"
CREATE TABLE IF NOT EXISTS usage_snapshots (
 vendor TEXT NOT NULL, account TEXT NOT NULL, pool TEXT NOT NULL,
 fetched_at REAL NOT NULL, payload TEXT NOT NULL,
 PRIMARY KEY(vendor,account,pool)
);
CREATE INDEX IF NOT EXISTS sessions_updated ON sessions(updated_at);
"#;

/// Version 26: scheduled automations and their run history
/// (`store::automations`). Idempotent like every step.
const MIGRATION_26: &str = r#"
CREATE TABLE IF NOT EXISTS automations (
 id TEXT PRIMARY KEY, workspace TEXT NOT NULL, name TEXT NOT NULL,
 payload TEXT NOT NULL, paused INTEGER NOT NULL DEFAULT 0, next_run_at REAL,
 created_at REAL NOT NULL, updated_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS automations_workspace ON automations(workspace,created_at);
CREATE INDEX IF NOT EXISTS automations_due ON automations(paused,next_run_at);
CREATE TABLE IF NOT EXISTS automation_runs (
 id TEXT PRIMARY KEY, automation_id TEXT NOT NULL, status TEXT NOT NULL,
 origin TEXT NOT NULL, scheduled_for REAL, started_at REAL NOT NULL,
 finished_at REAL, session_id TEXT, job_id TEXT, payload TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS automation_runs_automation ON automation_runs(automation_id,started_at);
"#;

pub struct Store {
    pub path: PathBuf,
    connection: Mutex<Connection>,
}

/// One persisted provider usage payload (raw official data, e.g. the Codex
/// `account/rateLimits/read` result) with the time it was fetched.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageRow {
    pub vendor: String,
    pub account: String,
    pub pool: String,
    pub fetched_at: f64,
    pub payload: Value,
}

impl Store {
    pub fn upsert_usage_snapshot(&self, row: &UsageRow) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO usage_snapshots(vendor,account,pool,fetched_at,payload) VALUES(?,?,?,?,?) ON CONFLICT(vendor,account,pool) DO UPDATE SET fetched_at=excluded.fetched_at,payload=excluded.payload",
            params![row.vendor, row.account, row.pool, row.fetched_at, row.payload.to_string()],
        )?;
        Ok(())
    }
    pub fn usage_snapshots(&self) -> Result<Vec<UsageRow>> {
        let db = self.lock()?;
        let mut statement = db.prepare(
            "SELECT vendor,account,pool,fetched_at,payload FROM usage_snapshots ORDER BY fetched_at DESC",
        )?;
        let rows = statement
            .query_map([], |r| {
                Ok(UsageRow {
                    vendor: r.get(0)?,
                    account: r.get(1)?,
                    pool: r.get(2)?,
                    fetched_at: r.get(3)?,
                    payload: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or(Value::Null),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    /// Forget usage for a vendor (disconnect, account switch).
    pub fn delete_usage_snapshots(&self, vendor: &str) -> Result<usize> {
        Ok(self
            .lock()?
            .execute("DELETE FROM usage_snapshots WHERE vendor=?", [vendor])?)
    }
    /// `session_meta` rows whose key starts with `prefix`, as (key, value).
    pub fn session_meta_prefixed(
        &self,
        session_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, String)>> {
        let db = self.lock()?;
        let pattern = format!("{}%", prefix.replace('%', "\\%"));
        let mut statement = db.prepare(
            "SELECT key,value FROM session_meta WHERE session_id=? AND key LIKE ? ESCAPE '\\' ORDER BY key",
        )?;
        let rows = statement
            .query_map(params![session_id, pattern], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    /// Job records of one conversation, oldest first (at most `limit`, the
    /// most recent ones).
    pub fn session_jobs(&self, session_id: &str, limit: usize) -> Result<Vec<Value>> {
        let mut rows: Vec<Value> = self
            .query(
                "SELECT payload FROM desktop_jobs WHERE json_extract(payload,'$.session_id')=? ORDER BY rowid DESC LIMIT ?",
                params![session_id, limit.clamp(1, 1000)],
            )?
            .into_iter()
            .map(|row| row["payload"].clone())
            .collect();
        rows.reverse();
        Ok(rows)
    }
    /// Workspace paths a set of tasks changed, from vendor `files.changed`
    /// events and native file-change records.
    pub fn changed_files(&self, task_ids: &[String]) -> Result<Vec<String>> {
        let mut paths = Vec::new();
        for task in task_ids {
            for row in self.query(
                "SELECT payload FROM events WHERE task_id=? AND type='files.changed'",
                [task],
            )? {
                let payload: Value = match &row["payload"] {
                    Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
                    other => other.clone(),
                };
                for path in payload["paths"].as_array().into_iter().flatten() {
                    if let Some(path) = path.as_str() {
                        paths.push(path.to_owned());
                    }
                }
            }
            for row in self.query("SELECT path FROM file_changes WHERE task_id=?", [task])? {
                if let Some(path) = row["path"].as_str() {
                    paths.push(path.to_owned());
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        paths.retain(|p| seen.insert(p.clone()));
        Ok(paths)
    }
}

impl Store {
    /// Small per-session key/value used for execution targets and vendor
    /// session ids (`native_session:<vendor>`).
    pub fn set_session_meta(&self, session_id: &str, key: &str, value: &str) -> Result<()> {
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO session_meta(session_id,key,value) VALUES(?,?,?) ON CONFLICT(session_id,key) DO UPDATE SET value=excluded.value",
            params![session_id, key, value],
        )?;
        Ok(())
    }
    pub fn session_meta(&self, session_id: &str, key: &str) -> Result<Option<String>> {
        let connection = self.lock()?;
        let value = connection
            .query_row(
                "SELECT value FROM session_meta WHERE session_id=? AND key=?",
                params![session_id, key],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(value)
    }
    pub fn delete_session_meta(&self, session_id: &str, key: &str) -> Result<()> {
        self.lock()?.execute(
            "DELETE FROM session_meta WHERE session_id=? AND key=?",
            params![session_id, key],
        )?;
        Ok(())
    }
    pub fn clear_session_meta_prefix(&self, key_prefix: &str) -> Result<usize> {
        let connection = self.lock()?;
        let pattern = format!("{}%", key_prefix.replace('%', "\\%"));
        let count = connection.execute(
            "DELETE FROM session_meta WHERE key LIKE ? ESCAPE '\\'",
            params![pattern],
        )?;
        Ok(count)
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let existed = path.exists();
        // Downgrade protection: a database from a newer version is refused
        // from its file header, before SQLite opens it for writing, so the
        // newer version finds its data exactly as it left it.
        if let Some(version) = header_user_version(path).filter(|v| *v > SCHEMA_VERSION) {
            let writer = open_immutable(path).ok().and_then(|db| last_writer(&db));
            return Err(newer_database(version, writer));
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(10))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            // Only when the header was stale (a newer version's unmerged log).
            return Err(newer_database(version, last_writer(&connection)));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        connection.pragma_update(None, "foreign_keys", true)?;
        if version < SCHEMA_VERSION {
            if existed {
                let backup_path = path.with_extension(format!("pre-native-{}.sqlite", id()));
                let mut backup = Connection::open(&backup_path)?;
                rusqlite::backup::Backup::new(&connection, &mut backup)?.run_to_completion(
                    128,
                    Duration::from_millis(5),
                    None,
                )?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o600))?;
                }
            }
            let tx = connection.transaction()?;
            if version < 24 {
                migrate_to_24(&tx)?;
            }
            // Ordered forward steps; each is idempotent.
            if version < 25 {
                tx.execute_batch(MIGRATION_25)?;
            }
            if version < 26 {
                tx.execute_batch(MIGRATION_26)?;
            }
            if version < 27 {
                tx.execute_batch(MIGRATION_27)?;
            }
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        // The last version that opened this profile, so an older version
        // that refuses it can name the version to use. Best effort: it only
        // improves a message.
        if let Err(error) = connection.execute(
            "INSERT INTO native_meta(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE value<>excluded.value",
            params![keys::APP_VERSION, crate::VERSION],
        ) {
            tracing::warn!("could not record the app version: {error}");
        }
        let store = Self {
            path: path.into(),
            connection: Mutex::new(connection),
        };
        store.import_legacy_goals()?;
        store.import_legacy_background()?;
        Ok(store)
    }
}

/// `PRAGMA user_version` read from the file header (bytes 60–63), or
/// `None` for a missing, short or non-SQLite file.
fn header_user_version(path: &Path) -> Option<i64> {
    use std::io::Read;
    let mut header = [0u8; 64];
    fs::File::open(path).ok()?.read_exact(&mut header).ok()?;
    (&header[..16] == b"SQLite format 3\0").then(|| {
        i64::from(i32::from_be_bytes([
            header[60], header[61], header[62], header[63],
        ]))
    })
}

/// Open a SQLite file so that nothing is ever written to it: no journal
/// recovery, no write-ahead log, no locks.
pub(crate) fn open_immutable(path: &Path) -> Result<Connection> {
    let mut uri = String::from("file:");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str("?immutable=1");
    Ok(Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_URI
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

fn last_writer(db: &Connection) -> Option<String> {
    db.query_row(
        "SELECT value FROM native_meta WHERE key=?",
        [keys::APP_VERSION],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .filter(|v| v.len() <= 64 && v.chars().all(|c| c.is_ascii_graphic()))
}

fn newer_database(version: i64, writer: Option<String>) -> anyhow::Error {
    let writer = writer
        .map(|v| format!("ShadowCode {v}"))
        .unwrap_or_else(|| "a newer ShadowCode".into());
    anyhow::anyhow!(
        "This profile's database was last used by {writer} (database format {version}); this version ({}) reads formats up to {SCHEMA_VERSION}. Nothing was changed. Open it with that version or newer, or restore a backup made by this version: run `shadowcode restore PATH` in a terminal.",
        crate::VERSION
    )
}

/// The flat pre-0.28 schema (user_version 24): every table plus the columns
/// older databases lacked.
fn migrate_to_24(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(SCHEMA)?;
    for (table, column, kind) in [
        ("sessions", "title", "TEXT"),
        ("sessions", "usage_json", "TEXT"),
        ("sessions", "parent_id", "TEXT"),
        ("sessions", "branched_at", "REAL"),
        ("tasks", "usage_json", "TEXT"),
        ("file_changes", "observed_hash", "TEXT"),
        ("milestones", "mode", "TEXT NOT NULL DEFAULT 'code'"),
        (
            "milestones",
            "require_verification",
            "INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        let columns: Vec<String> = tx
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |r| r.get(1))?
            .collect::<rusqlite::Result<_>>()?;
        if !columns.iter().any(|c| c == column) {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))?;
        }
    }
    Ok(())
}

impl Store {
    fn import_legacy_goals(&self) -> Result<()> {
        let legacy = self.path.with_file_name("goals.db");
        if legacy == self.path || !legacy.is_file() {
            return Ok(());
        }
        let mut db = self.lock()?;
        if db
            .query_row(
                "SELECT value FROM native_meta WHERE key=?",
                [keys::LEGACY_GOALS_IMPORTED],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .is_some()
        {
            return Ok(());
        }
        db.execute(
            "ATTACH DATABASE ? AS legacy_goals",
            params![legacy.to_string_lossy()],
        )?;
        let result = (|| -> Result<()> {
            let tx = db.transaction()?;
            tx.execute_batch(
                "INSERT OR IGNORE INTO goals SELECT * FROM legacy_goals.goals;
                INSERT OR IGNORE INTO milestones(id,goal_id,title,status,order_index,detail,task_id,created_at,updated_at)
                    SELECT id,goal_id,title,status,order_index,detail,task_id,created_at,updated_at FROM legacy_goals.milestones;",
            )?;
            tx.execute(
                "INSERT INTO native_meta(key,value) VALUES(?,'true')",
                [keys::LEGACY_GOALS_IMPORTED],
            )?;
            tx.commit()?;
            Ok(())
        })();
        db.execute("DETACH DATABASE legacy_goals", [])?;
        result
    }

    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        match self.connection.lock() {
            Ok(connection) => Ok(connection),
            // A panic while the connection was held (a bug in one task) must
            // not make every later task and the conversation's final state
            // unsaveable. SQLite keeps each statement atomic; roll back a
            // transaction the panicking code left open, then carry on.
            Err(poisoned) => {
                let connection = poisoned.into_inner();
                self.connection.clear_poison();
                if !connection.is_autocommit() {
                    connection
                        .execute_batch("ROLLBACK")
                        .context("Could not roll back after an internal error")?;
                }
                tracing::warn!("Recovered the database connection after a panic");
                Ok(connection)
            }
        }
    }
    /// Run database work on tokio's blocking pool. Async code uses this for
    /// writes and large reads: they wait on the connection lock and on
    /// `fsync` (synchronous=FULL), which must not stall an async worker that
    /// is also streaming a model reply. A panic resumes in the caller.
    pub async fn run<T, F>(self: &Arc<Self>, work: F) -> Result<T>
    where
        F: FnOnce(&Store) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let store = self.clone();
        match tokio::task::spawn_blocking(move || work(&store)).await {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => Err(anyhow::anyhow!("Database work stopped: {error}")),
        }
    }
    pub(crate) fn query(&self, sql: &str, args: impl Params) -> Result<Vec<Value>> {
        query_rows(&*self.lock()?, sql, args)
    }
    pub(crate) fn execute(&self, sql: &str, args: impl Params) -> Result<usize> {
        Ok(self.lock()?.execute(sql, args)?)
    }

    pub fn create_session(&self, workspace: &Path, model: &str, title: &str) -> Result<Value> {
        let sid = id();
        let now = now();
        self.execute("INSERT INTO sessions(id,workspace,created_at,updated_at,model_id,status,title) VALUES(?,?,?,?,?,'active',?)",
            params![sid,workspace.to_string_lossy(),now,now,model,title])?;
        self.session(&sid)?.context("Created session disappeared")
    }
    /// The session row, with `usage` (tokens and cost of its finished
    /// tasks) parsed from `usage_json`.
    pub fn session(&self, sid: &str) -> Result<Option<Value>> {
        Ok(self
            .query("SELECT * FROM sessions WHERE id=?", [sid])?
            .into_iter()
            .next()
            .map(|mut row| {
                row["usage"] = json!(crate::usage::parse(&row["usage_json"]));
                row
            }))
    }
    /// Resolve identifiers against the complete indexed history, never a recent
    /// list of potentially large job payloads. Two rows suffice for ambiguity.
    pub fn resolve_id(&self, kind: &str, prefix: &str) -> Result<String> {
        ensure!(
            !prefix.is_empty()
                && prefix.len() <= 128
                && prefix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
            "Provide a valid ID or ID prefix"
        );
        let table = match kind {
            "session" => "sessions",
            "job" => "desktop_jobs",
            _ => anyhow::bail!("Unknown identifier kind"),
        };
        let rows = self.query(
            &format!("SELECT id FROM {table} WHERE id>=? AND id<? ORDER BY id LIMIT 2"),
            params![prefix, format!("{prefix}~")],
        )?;
        ensure!(!rows.is_empty(), "No {kind} matches {prefix}");
        ensure!(
            rows.len() == 1,
            "Choose a unique {kind} ID prefix; at least two matches found"
        );
        Ok(rows[0]["id"]
            .as_str()
            .context("Stored identifier missing")?
            .into())
    }
    pub fn sessions(&self, search: &str, limit: usize) -> Result<Vec<Value>> {
        self.sessions_in(search, limit, None)
    }
    pub fn sessions_in(
        &self,
        search: &str,
        limit: usize,
        workspace: Option<&Path>,
    ) -> Result<Vec<Value>> {
        let needle = format!(
            "%{}%",
            search
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_")
        );
        self.query("SELECT s.* FROM sessions s WHERE (? IS NULL OR s.workspace=?) AND (s.title LIKE ? ESCAPE '!' OR s.workspace LIKE ? ESCAPE '!'
            OR EXISTS(SELECT 1 FROM tasks t WHERE t.session_id=s.id AND t.prompt LIKE ? ESCAPE '!'))
            ORDER BY s.updated_at DESC LIMIT ?", params![workspace.map(|p|p.to_string_lossy()),workspace.map(|p|p.to_string_lossy()),needle,needle,needle,limit.clamp(1,10000)])
    }
    /// The session list the app shows. Compare lane conversations (sessions
    /// with a `compare_id` meta row) are left out unless `include_compare`;
    /// every row carries `compare_id` and `compare_lane` (null for ordinary
    /// conversations). Subagent conversations are always left out here; see
    /// `sessions_listed_with`. Second-opinion reviewers' conversations
    /// (`second_opinion` meta) are never listed. A worktree task's conversation carries
    /// `worktree_task` and `worktree_source` (its project) and is listed under
    /// that project.
    pub fn sessions_listed(
        &self,
        search: &str,
        limit: usize,
        workspace: Option<&Path>,
        include_compare: bool,
    ) -> Result<Vec<Value>> {
        self.sessions_listed_with(search, limit, workspace, include_compare, false)
    }
    /// `sessions_listed`, optionally including subagent conversations
    /// (sessions with a `subagent_parent` meta row). Every row carries
    /// `subagent_parent` (null for ordinary conversations).
    pub fn sessions_listed_with(
        &self,
        search: &str,
        limit: usize,
        workspace: Option<&Path>,
        include_compare: bool,
        include_subagents: bool,
    ) -> Result<Vec<Value>> {
        let needle = format!(
            "%{}%",
            search
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_")
        );
        self.query("SELECT s.*,
            (SELECT value FROM session_meta m WHERE m.session_id=s.id AND m.key='compare_id') AS compare_id,
            (SELECT value FROM session_meta m WHERE m.session_id=s.id AND m.key='compare_lane') AS compare_lane,
            (SELECT value FROM session_meta m WHERE m.session_id=s.id AND m.key='worktree_task') AS worktree_task,
            (SELECT value FROM session_meta m WHERE m.session_id=s.id AND m.key='worktree_source') AS worktree_source,
            (SELECT value FROM session_meta m WHERE m.session_id=s.id AND m.key='subagent_parent') AS subagent_parent
            FROM sessions s WHERE (? IS NULL OR s.workspace=?
                OR EXISTS(SELECT 1 FROM session_meta w WHERE w.session_id=s.id AND w.key='worktree_source' AND w.value=?))
            AND (s.title LIKE ? ESCAPE '!' OR s.workspace LIKE ? ESCAPE '!'
            OR EXISTS(SELECT 1 FROM tasks t WHERE t.session_id=s.id AND t.prompt LIKE ? ESCAPE '!'))
            AND (? OR NOT EXISTS(SELECT 1 FROM session_meta c WHERE c.session_id=s.id AND c.key='compare_id'))
            AND (? OR NOT EXISTS(SELECT 1 FROM session_meta a WHERE a.session_id=s.id AND a.key='subagent_parent'))
            AND NOT EXISTS(SELECT 1 FROM session_meta o WHERE o.session_id=s.id AND o.key='second_opinion')
            ORDER BY s.updated_at DESC LIMIT ?", params![workspace.map(|p|p.to_string_lossy()),workspace.map(|p|p.to_string_lossy()),workspace.map(|p|p.to_string_lossy()),needle,needle,needle,include_compare,include_subagents,limit.clamp(1,10000)])
    }
    /// Move a conversation to another folder (a worktree task's conversation
    /// returns to its project when the worktree is removed). The vendor CLI
    /// session ids are forgotten: they belong to the old folder.
    pub fn move_session(&self, sid: &str, workspace: &Path) -> Result<()> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        ensure!(
            tx.execute(
                "UPDATE sessions SET workspace=?,updated_at=? WHERE id=?",
                params![workspace.to_string_lossy(), now(), sid]
            )? == 1,
            "Session not found"
        );
        tx.execute(
            "DELETE FROM session_meta WHERE session_id=? AND key LIKE 'native\\_session:%' ESCAPE '\\'",
            [sid],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn rename_session(&self, sid: &str, title: &str) -> Result<()> {
        ensure!(title.len() <= 500, "Task title is too long");
        ensure!(
            self.execute(
                "UPDATE sessions SET title=?,updated_at=? WHERE id=?",
                params![title, now(), sid]
            )? == 1,
            "Session not found"
        );
        Ok(())
    }
    pub fn branch_session(&self, sid: &str, title: &str) -> Result<Value> {
        self.branch_session_with_memory(sid, title, "")
    }

    /// Fork session state from a checkpoint/event id into a new session branch
    /// without deleting the original. Copies only events with id <= event_id.
    pub fn fork_session_from_event(&self, sid: &str, event_id: i64, title: &str) -> Result<Value> {
        ensure!(event_id > 0, "event_id must be positive");
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let parent = query_rows(&tx, "SELECT * FROM sessions WHERE id=?", [sid])?
            .pop()
            .context("Session not found")?;
        let selected: i64 = tx.query_row(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND id=?",
            params![sid, event_id],
            |row| row.get(0),
        )?;
        ensure!(selected == 1, "event_id does not belong to this session");
        // Reuse only the tape of a task completed at or before the cut. A newer
        // tape would leak future turns into the fork; events alone lose tool results.
        let completed = query_rows(&tx,
            "SELECT e.id,e.task_id FROM events e WHERE e.session_id=? AND e.id<=? AND e.type='agent.completed' ORDER BY e.id DESC LIMIT 1",
            params![sid,event_id])?;
        let mut seed: Vec<Value> = Vec::new();
        let mut after = 0;
        if let Some(event) = completed.first() {
            let tape = query_rows(&tx,
                "SELECT payload FROM job_messages WHERE job_id=(SELECT id FROM desktop_jobs WHERE json_extract(payload,'$.task_id')=? ORDER BY rowid DESC LIMIT 1) ORDER BY ordinal",
                [event["task_id"].as_str().unwrap_or("")])?;
            if !tape.is_empty() {
                seed = tape.into_iter().map(|row| row["payload"].clone()).collect();
                after = event["id"].as_i64().unwrap_or(0);
            }
        }
        // A mid-turn cut contains only observed text. Do not reconstruct or
        // replay unfinished tool calls from transcript fragments.
        for event in query_rows(&tx,
            "SELECT type,payload FROM events WHERE session_id=? AND id>? AND id<=? AND type IN ('user.message','model.delta','agent.message') ORDER BY id",
            params![sid,after,event_id])? {
            let kind = event["type"].as_str().unwrap_or("");
            if kind == "model.delta" && event["payload"]["complete"] != true { continue; }
            if let Some(text) = event["payload"]["text"].as_str() {
                seed.push(json!({"role":if kind=="user.message" {"user"} else {"assistant"},"content":text}));
            }
        }
        crate::context::repair_incomplete(&mut seed);
        let branch = id();
        let time = now();
        let title = if title.is_empty() {
            format!(
                "{} (fork@{event_id})",
                parent["title"].as_str().unwrap_or("Task")
            )
        } else {
            title.into()
        };
        tx.execute(
            "INSERT INTO sessions(id,workspace,created_at,updated_at,model_id,status,title,parent_id,branched_at) VALUES(?,?,?,?,?,'active',?,?,?)",
            params![
                branch,
                parent["workspace"].as_str(),
                time,
                time,
                parent["model_id"].as_str(),
                title,
                sid,
                time
            ],
        )?;
        tx.execute(
            "INSERT INTO events(ts,type,session_id,task_id,payload) SELECT ts,type,?,task_id,payload FROM events WHERE session_id=? AND id<=? ORDER BY id",
            params![branch, sid, event_id],
        )?;
        tx.execute(
            "INSERT INTO session_meta(session_id,key,value) VALUES(?,'forked_from_event',?)",
            params![branch, event_id.to_string()],
        )?;
        let result = query_rows(&tx, "SELECT * FROM sessions WHERE id=?", [&branch])?
            .pop()
            .context("Fork not found")?;
        tx.execute(
            "INSERT INTO session_meta(session_id,key,value) VALUES(?,'message_seed',?)",
            params![branch, serde_json::to_string(&seed)?],
        )?;
        let original = query_rows(&tx, "SELECT * FROM sessions WHERE id=?", [sid])?
            .pop()
            .context("Original session missing after fork")?;
        tx.commit()?;
        Ok(json!({
            "fork": result,
            "original": original,
            "forked_from_event": event_id,
            "original_intact": true
        }))
    }
    /// Fork keeping only what came before the task `event_id` belongs to
    /// (its prompt included), for Edit & resend. The event must belong to
    /// the session. With nothing earlier the fork starts empty in the same
    /// project.
    pub fn fork_session_before_event(
        &self,
        sid: &str,
        event_id: i64,
        title: &str,
    ) -> Result<Value> {
        let owned = self.query(
            "SELECT id,task_id FROM events WHERE session_id=? AND id=?",
            params![sid, event_id],
        )?;
        let event = owned
            .first()
            .context("event_id does not belong to this session")?;
        // The task's first event (a queued follow-up records its prompt
        // before it starts).
        let cut = match event["task_id"].as_str().filter(|t| !t.is_empty()) {
            Some(task) => self
                .query(
                    "SELECT MIN(id) AS id FROM events WHERE session_id=? AND task_id=?",
                    params![sid, task],
                )?
                .first()
                .and_then(|row| row["id"].as_i64())
                .unwrap_or(event_id),
            None => event_id,
        };
        let earlier = self.query(
            "SELECT id FROM events WHERE session_id=? AND id<? ORDER BY id DESC LIMIT 1",
            params![sid, cut],
        )?;
        if let Some(previous) = earlier.first().and_then(|row| row["id"].as_i64()) {
            return self.fork_session_from_event(sid, previous, title);
        }
        let parent = self.session(sid)?.context("Session not found")?;
        let branch = id();
        let time = now();
        let title = if title.is_empty() {
            format!("{} (edited)", parent["title"].as_str().unwrap_or("Task"))
        } else {
            title.into()
        };
        self.execute(
            "INSERT INTO sessions(id,workspace,created_at,updated_at,model_id,status,title,parent_id,branched_at) VALUES(?,?,?,?,?,'active',?,?,?)",
            params![
                branch,
                parent["workspace"].as_str(),
                time,
                time,
                parent["model_id"].as_str(),
                title,
                sid,
                time
            ],
        )?;
        Ok(json!({
            "fork": self.session(&branch)?.context("Fork not found")?,
            "original": parent,
            "forked_from_event": null,
            "original_intact": true
        }))
    }
    pub fn branch_session_with_memory(
        &self,
        sid: &str,
        title: &str,
        memory: &str,
    ) -> Result<Value> {
        ensure!(
            memory.len() <= 32_000_000,
            "Branch memory exceeds its limit"
        );
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let parent = query_rows(&tx, "SELECT * FROM sessions WHERE id=?", [sid])?
            .pop()
            .context("Session not found")?;
        let branch = id();
        let time = now();
        let title = if title.is_empty() {
            format!("{} (branch)", parent["title"].as_str().unwrap_or("Task"))
        } else {
            title.into()
        };
        tx.execute("INSERT INTO sessions(id,workspace,created_at,updated_at,model_id,status,title,parent_id,branched_at) VALUES(?,?,?,?,?,'active',?,?,?)",
            params![branch,parent["workspace"].as_str(),time,time,parent["model_id"].as_str(),title,sid,time])?;
        tx.execute("INSERT INTO events(ts,type,session_id,task_id,payload) SELECT ts,type,?,task_id,payload FROM events WHERE session_id=? ORDER BY id",params![branch,sid])?;
        tx.execute("INSERT INTO pins(session_id,task_id,ts,label,body) SELECT ?,task_id,ts,label,body FROM pins WHERE session_id=?",params![branch,sid])?;
        let tape=query_rows(&tx,"SELECT payload FROM job_messages WHERE job_id=(SELECT id FROM desktop_jobs WHERE json_extract(payload,'$.session_id')=? AND EXISTS(SELECT 1 FROM job_messages WHERE job_id=desktop_jobs.id) ORDER BY rowid DESC LIMIT 1) ORDER BY ordinal",[sid])?;
        if !tape.is_empty() {
            let messages: Vec<_> = tape.into_iter().map(|r| r["payload"].clone()).collect();
            tx.execute(
                "INSERT INTO session_meta(session_id,key,value) VALUES(?,'message_seed',?)",
                params![branch, json!(messages).to_string()],
            )?;
        } else {
            tx.execute("INSERT INTO session_meta(session_id,key,value) SELECT ?,key,value FROM session_meta WHERE session_id=? AND key='message_seed'",params![branch,sid])?;
        }
        if !memory.is_empty() {
            tx.execute(
                "INSERT INTO session_meta(session_id,key,value) VALUES(?,'memory_seed',?)",
                params![branch, memory],
            )?;
        }
        let result = query_rows(&tx, "SELECT * FROM sessions WHERE id=?", [&branch])?
            .pop()
            .context("Branch not found")?;
        tx.commit()?;
        Ok(result)
    }
    pub fn delete_session(&self, sid: &str) -> Result<bool> {
        Ok(self.delete_session_tree(sid)?.is_some())
    }
    /// Delete a conversation together with its subagent conversations (at
    /// any depth) and their run records, in one transaction. Subagent
    /// conversations are hidden from the sidebar, so they would otherwise be
    /// left unreachable. A subagent conversation that a fork of this
    /// conversation still shows (its `subagent.started` card was copied into
    /// the fork) moves to that fork instead. Forks themselves are kept.
    /// Returns the deleted subagent run ids (their saved patches are files
    /// the caller removes), or `None` when the conversation did not exist.
    pub fn delete_session_tree(&self, sid: &str) -> Result<Option<Vec<String>>> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let mut doomed = vec![sid.to_owned()];
        let mut next = 0;
        while next < doomed.len() {
            let parent = doomed[next].clone();
            next += 1;
            let children: Vec<String> = tx
                .prepare("SELECT session_id FROM session_meta WHERE key=? AND value=?")?
                .query_map(params![keys::SUBAGENT_PARENT, parent], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for child in children {
                if doomed.contains(&child) {
                    continue;
                }
                let adopters: Vec<String> = tx
                    .prepare(
                        "SELECT session_id FROM events WHERE type='subagent.started'
                         AND json_extract(payload,'$.session_id')=? AND session_id<>?
                         AND session_id IN (SELECT id FROM sessions) ORDER BY id DESC",
                    )?
                    .query_map(params![child, parent], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                let adopter = adopters.into_iter().find(|s| !doomed.contains(s));
                match adopter {
                    Some(fork) => {
                        tx.execute(
                            "UPDATE session_meta SET value=? WHERE session_id=? AND key=?",
                            params![fork, child, keys::SUBAGENT_PARENT],
                        )?;
                        // The run record moves too, so deleting the fork
                        // later removes it.
                        let run: Option<String> = tx
                            .query_row(
                                "SELECT value FROM session_meta WHERE session_id=? AND key=?",
                                params![child, keys::SUBAGENT_RUN],
                                |r| r.get(0),
                            )
                            .optional()?;
                        if let Some(run) = run {
                            let index = keys::subagent_index(&fork);
                            let mut ids: Vec<String> = tx
                                .query_row(
                                    "SELECT value FROM native_meta WHERE key=?",
                                    [&index],
                                    |r| r.get::<_, String>(0),
                                )
                                .optional()?
                                .and_then(|text| serde_json::from_str(&text).ok())
                                .unwrap_or_default();
                            if !ids.contains(&run) {
                                ids.push(run);
                                tx.execute(
                                    "INSERT INTO native_meta(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                                    params![index, serde_json::to_string(&ids)?],
                                )?;
                            }
                        }
                    }
                    None => doomed.push(child),
                }
            }
        }
        // Second-opinion reviewers' conversations go with the conversation
        // they belong to; one still running is left for its record's pruning.
        for parent in doomed.clone() {
            let reviews: Vec<String> = tx
                .prepare("SELECT session_id FROM session_meta WHERE key=? AND value=?")?
                .query_map(params![keys::SECOND_OPINION_OF, parent], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for review in reviews {
                let active: i64 = tx.query_row("SELECT count(*) FROM desktop_jobs WHERE json_extract(payload,'$.session_id')=? AND json_extract(payload,'$.status') IN ('queued','running','paused','cancelling')",[&review],|r|r.get(0))?;
                if active == 0 && !doomed.contains(&review) {
                    doomed.push(review);
                }
            }
        }
        let mut runs = Vec::new();
        for session in &doomed {
            let active: i64 = tx.query_row("SELECT count(*) FROM desktop_jobs WHERE json_extract(payload,'$.session_id')=? AND json_extract(payload,'$.status') IN ('queued','running','paused','cancelling')",[session],|r|r.get(0))?;
            ensure!(
                active == 0,
                "Stop the running task before deleting this session"
            );
            let index = keys::subagent_index(session);
            let ids: Vec<String> = tx
                .query_row("SELECT value FROM native_meta WHERE key=?", [&index], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            for id in ids {
                let key = keys::subagent_run(&id);
                let run_session = tx
                    .query_row("SELECT value FROM native_meta WHERE key=?", [&key], |r| {
                        r.get::<_, String>(0)
                    })
                    .optional()?
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                    .and_then(|run| run["session_id"].as_str().map(str::to_owned));
                // A run whose conversation moved to a fork stays readable.
                if run_session.is_some_and(|s| !s.is_empty() && !doomed.contains(&s)) {
                    continue;
                }
                tx.execute("DELETE FROM native_meta WHERE key=?", [&key])?;
                runs.push(id);
            }
            tx.execute("DELETE FROM native_meta WHERE key=?", [&index])?;
            for table in ["events", "pins", "session_meta", "queued_tasks"] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE session_id=?"),
                    [session],
                )?;
            }
            tx.execute(
                "DELETE FROM file_changes WHERE task_id IN (SELECT id FROM tasks WHERE session_id=?)",
                [session],
            )?;
            tx.execute("DELETE FROM job_messages WHERE job_id IN (SELECT id FROM desktop_jobs WHERE json_extract(payload,'$.session_id')=?)",[session])?;
            tx.execute(
                "DELETE FROM desktop_jobs WHERE json_extract(payload,'$.session_id')=?",
                [session],
            )?;
            tx.execute("DELETE FROM tasks WHERE session_id=?", [session])?;
            tx.execute(
                "UPDATE sessions SET parent_id=NULL WHERE parent_id=?",
                [session],
            )?;
        }
        let deleted = tx.execute("DELETE FROM sessions WHERE id=?", [sid])? != 0;
        for child in &doomed[1..] {
            tx.execute("DELETE FROM sessions WHERE id=?", [child])?;
        }
        tx.commit()?;
        Ok(deleted.then_some(runs))
    }
    pub fn create_task(&self, sid: &str, prompt: &str) -> Result<String> {
        let task = id();
        self.execute(
            "INSERT INTO tasks(id,session_id,prompt,status,created_at) VALUES(?,?,?,'running',?)",
            params![task, sid, prompt, now()],
        )?;
        Ok(task)
    }
    pub fn finish_task(&self, tid: &str, status: &str, summary: &str, usage: &Value) -> Result<()> {
        self.finish_transaction(tid, status, summary, usage, None)
            .map(|_| ())
    }
    pub fn finish_job(&self, job: &mut Value) -> Result<Value> {
        let tid = job["task_id"]
            .as_str()
            .context("Missing task ID")?
            .to_owned();
        let status = job["status"].as_str().context("Missing status")?.to_owned();
        let summary = job["summary"].as_str().unwrap_or("").to_owned();
        let usage = job["usage"].clone();
        self.finish_transaction(&tid, &status, &summary, &usage, Some(job))?
            .context("Missing completion event")
    }
    fn finish_transaction(
        &self,
        tid: &str,
        status: &str,
        summary: &str,
        usage: &Value,
        job: Option<&mut Value>,
    ) -> Result<Option<Value>> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let sid = finish_task_on(&tx, tid, status, summary, usage)?;
        let event = if let Some(job) = job {
            let ts = now();
            let payload = job["result"].clone();
            tx.execute("INSERT INTO events(ts,type,session_id,task_id,payload) VALUES(?,'agent.completed',?,?,?)",params![ts,sid,tid,payload.to_string()])?;
            let cursor = tx.last_insert_rowid();
            job["event_cursor"] = json!(cursor);
            tx.execute(
                "UPDATE desktop_jobs SET payload=? WHERE id=?",
                params![job.to_string(), job["id"].as_str()],
            )?;
            Some(
                json!({"id":cursor,"ts":ts,"type":"agent.completed","session_id":sid,"task_id":tid,"payload":payload}),
            )
        } else {
            None
        };
        tx.commit()?;
        Ok(event)
    }
    pub fn tasks(&self, sid: &str, limit: usize) -> Result<Vec<Value>> {
        self.query(
            "SELECT * FROM tasks WHERE session_id=? ORDER BY created_at DESC LIMIT ?",
            params![sid, limit.clamp(1, 10000)],
        )
    }
    pub fn task(&self, tid: &str) -> Result<Option<Value>> {
        Ok(self.query("SELECT * FROM tasks WHERE id=?", [tid])?.pop())
    }
    pub fn last_task_event(&self, tid: &str, kind: &str) -> Result<Option<Value>> {
        Ok(self
            .query(
                "SELECT * FROM events WHERE task_id=? AND type=? ORDER BY id DESC LIMIT 1",
                params![tid, kind],
            )?
            .pop())
    }
    /// Keep observed receipts in a terminal result even when the worker never
    /// reached its final assessment. Reads only this task's verification data,
    /// not its potentially large tool-output or streaming history.
    pub(crate) fn task_verification(&self, tid: &str) -> Result<Value> {
        task_verification_on(&*self.lock()?, tid)
    }
    pub fn add_event(
        &self,
        kind: &str,
        payload: &Value,
        sid: Option<&str>,
        tid: Option<&str>,
    ) -> Result<Value> {
        let db = self.lock()?;
        let time = now();
        db.execute(
            "INSERT INTO events(ts,type,session_id,task_id,payload) VALUES(?,?,?,?,?)",
            params![time, kind, sid, tid, payload.to_string()],
        )?;
        Ok(
            json!({"id":db.last_insert_rowid(),"ts":time,"type":kind,"session_id":sid,"task_id":tid,"payload":payload}),
        )
    }
    /// Local-only store metrics for Doctor. No telemetry is sent.
    pub fn local_stats(&self) -> Result<Value> {
        let db = self.lock()?;
        let events: i64 = db.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
        let sessions: i64 = db.query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))?;
        let jobs: i64 = db.query_row("SELECT count(*) FROM desktop_jobs", [], |r| r.get(0))?;
        let page_count: i64 = db.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let page_size: i64 = db.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let wal = db
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap_or_default();
        let bytes = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        Ok(json!({
            "events": events,
            "sessions": sessions,
            "jobs": jobs,
            "bytes": bytes,
            "page_count": page_count,
            "page_size": page_size,
            "journal_mode": wal,
            "telemetry": false
        }))
    }
    pub fn event_cursor(&self, sid: &str) -> Result<i64> {
        Ok(self.lock()?.query_row(
            "SELECT coalesce(max(id),0) FROM events WHERE session_id=?",
            [sid],
            |r| r.get(0),
        )?)
    }
    pub fn events_after(
        &self,
        sid: &str,
        after: i64,
        through: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        self.query(
            "SELECT * FROM events WHERE session_id=? AND id>? AND id<=? ORDER BY id LIMIT ?",
            params![
                sid,
                after.max(0),
                through.unwrap_or(i64::MAX),
                limit.clamp(1, 2000)
            ],
        )
    }
    pub fn recent_events(&self, sid: &str, limit: usize) -> Result<Vec<Value>> {
        self.recent_events_through(sid, i64::MAX, limit)
    }
    pub fn recent_events_through(
        &self,
        sid: &str,
        through: i64,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let mut rows = self.query(
            "SELECT * FROM events WHERE session_id=? AND id<=? ORDER BY id DESC LIMIT ?",
            params![sid, through, limit.clamp(1, 10000)],
        )?;
        rows.reverse();
        Ok(rows)
    }
    /// A bounded desktop history page. Oversized events remain in the store/export.
    pub fn history_page(&self, sid: &str, through: i64) -> Result<Value> {
        let events = self.query(
            "WITH candidates AS (SELECT id,ts,session_id,task_id,
                CASE WHEN length(CAST(payload AS BLOB))>262144 THEN 'history.omitted' ELSE type END AS type,
                CASE WHEN length(CAST(payload AS BLOB))>262144 THEN json_object('text','A large saved event is omitted from this preview. Use Export this task as JSON in the command palette to read its original content.','original_type',type,'original_bytes',length(CAST(payload AS BLOB))) ELSE payload END AS payload
                FROM events WHERE session_id=? AND id<=? ORDER BY id DESC LIMIT 128),
             bounded AS (SELECT *,sum(length(CAST(payload AS BLOB))) OVER (ORDER BY id DESC) AS bytes FROM candidates)
             SELECT id,ts,session_id,task_id,type,payload FROM bounded WHERE bytes<=2097152 ORDER BY id",
            params![sid, through],
        )?;
        let first = events.first().and_then(|e| e["id"].as_i64()).unwrap_or(0);
        let has_older = first > 0
            && !self
                .query(
                    "SELECT id FROM events WHERE session_id=? AND id<? LIMIT 1",
                    params![sid, first],
                )?
                .is_empty();
        Ok(
            json!({"events":events,"first_cursor":first,"event_cursor":through,"has_older":has_older}),
        )
    }
    pub fn save_job(&self, job: &Value) -> Result<()> {
        let id = job["id"].as_str().context("Job requires an ID")?;
        self.execute("INSERT INTO desktop_jobs(id,payload) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload",params![id,job.to_string()])?;
        Ok(())
    }
    /// Queue the task and its recoverable job atomically.
    pub fn create_job(&self, job: &Value) -> Result<()> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let task_id = job["task_id"].as_str().context("Missing task ID")?;
        let session_id = job["session_id"].as_str().context("Missing session ID")?;
        let prompt = job["task"].as_str().context("Missing task text")?;
        tx.execute(
            "INSERT INTO tasks(id,session_id,prompt,status,created_at) VALUES(?,?,?,'queued',?)",
            params![task_id, session_id, prompt, now()],
        )?;
        tx.execute(
            "INSERT INTO desktop_jobs(id,payload) VALUES(?,?)",
            params![job["id"].as_str(), job.to_string()],
        )?;
        tx.execute(
            "INSERT INTO events(ts,type,session_id,task_id,payload) VALUES(?,'user.message',?,?,?)",
            params![
                now(),
                session_id,
                task_id,
                json!({"text":prompt}).to_string()
            ],
        )?;
        tx.execute("UPDATE sessions SET updated_at=?,title=CASE WHEN title IS NULL OR title='' OR title IN ('New task','Welcome') THEN ? ELSE title END WHERE id=?",params![now(),prompt.chars().take(80).collect::<String>(),session_id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn latest_session_messages(
        &self,
        session_id: &str,
        excluding_job: &str,
    ) -> Result<Vec<Value>> {
        let row=self.query("SELECT id FROM desktop_jobs WHERE json_extract(payload,'$.session_id')=? AND id!=? AND EXISTS(SELECT 1 FROM job_messages WHERE job_id=desktop_jobs.id) ORDER BY rowid DESC LIMIT 1",params![session_id,excluding_job])?;
        match row.first().and_then(|r| r["id"].as_str()) {
            Some(id) => self.messages(id),
            None => {
                let seed = self.query(
                    "SELECT value FROM session_meta WHERE session_id=? AND key='message_seed'",
                    [session_id],
                )?;
                match seed.first().and_then(|r| r["value"].as_str()) {
                    Some(seed) => Ok(serde_json::from_str(seed)?),
                    None => Ok(Vec::new()),
                }
            }
        }
    }
    pub fn jobs(&self, limit: usize) -> Result<Vec<Value>> {
        Ok(self
            .query(
                "SELECT payload FROM desktop_jobs ORDER BY rowid DESC LIMIT ?",
                [limit.clamp(1, 10000)],
            )?
            .into_iter()
            .map(|v| v["payload"].clone())
            .collect())
    }
    pub fn job(&self, id: &str) -> Result<Option<Value>> {
        Ok(self
            .query("SELECT payload FROM desktop_jobs WHERE id=?", [id])?
            .pop()
            .map(|v| v["payload"].clone()))
    }
    /// Keep active work visible even after unrelated projects produce a large
    /// amount of completed history. Insertion order is the workspace FIFO order.
    pub fn active_and_recent_jobs(&self, limit: usize) -> Result<Vec<Value>> {
        Ok(self.query(
            "SELECT payload FROM desktop_jobs WHERE rowid IN (SELECT rowid FROM desktop_jobs ORDER BY rowid DESC LIMIT ?) OR json_extract(payload,'$.status') IN ('queued','running','paused','cancelling') ORDER BY rowid DESC",
            [limit.clamp(1, 10000)],
        )?.into_iter().map(|row|row["payload"].clone()).collect())
    }
    /// Small polling records. Full prompts/results remain available by exact ID.
    /// `second_opinion` names the review a job runs (null for other jobs).
    pub fn job_summaries(&self, limit: usize) -> Result<Vec<Value>> {
        Ok(self.query(
            "SELECT json_object('id',id,'workspace',json_extract(payload,'$.workspace'),'session_id',json_extract(payload,'$.session_id'),'task_id',json_extract(payload,'$.task_id'),'status',json_extract(payload,'$.status'),'mode',json_extract(payload,'$.mode'),'purpose',substr(json_extract(payload,'$.routing.purpose'),1,32),'model',substr(json_extract(payload,'$.model'),1,512),'started_at',json_extract(payload,'$.started_at'),'finished_at',json_extract(payload,'$.finished_at'),'event_cursor',json_extract(payload,'$.event_cursor'),'task',substr(json_extract(payload,'$.task'),1,512),'task_truncated',CASE WHEN length(json_extract(payload,'$.task'))>512 THEN json('true') ELSE json('false') END,'second_opinion',(SELECT value FROM session_meta m WHERE m.session_id=json_extract(payload,'$.session_id') AND m.key='second_opinion')) AS payload FROM desktop_jobs WHERE rowid IN (SELECT rowid FROM desktop_jobs ORDER BY rowid DESC LIMIT ?) OR json_extract(payload,'$.status') IN ('queued','running','paused','cancelling') ORDER BY rowid DESC",
            [limit.clamp(1,100)],
        )?.into_iter().map(|row|row["payload"].clone()).collect())
    }
    pub fn current_job(&self, session: &str, include_finished: bool) -> Result<Option<Value>> {
        Ok(self.query(
            "SELECT payload FROM desktop_jobs WHERE json_extract(payload,'$.session_id')=? AND (? OR json_extract(payload,'$.status') IN ('queued','running','paused','cancelling')) ORDER BY CASE json_extract(payload,'$.status') WHEN 'running' THEN 0 WHEN 'paused' THEN 0 WHEN 'cancelling' THEN 1 WHEN 'queued' THEN 2 ELSE 3 END, CASE WHEN json_extract(payload,'$.status')='queued' THEN rowid END ASC, json_extract(payload,'$.finished_at') DESC, rowid DESC LIMIT 1",
            rusqlite::params![session,include_finished],
        )?.pop().map(|row|row["payload"].clone()))
    }
    /// Called only by the profile-lock owner, before accepting new work.
    pub fn recover_jobs(&self) -> Result<usize> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let jobs = query_rows(&tx,"SELECT payload FROM desktop_jobs WHERE json_extract(payload,'$.status') IN ('queued','running','paused','cancelling')",[])?;
        for row in &jobs {
            let mut job = row["payload"].clone();
            job["status"] = json!("interrupted");
            job["finished_at"] = json!(now());
            job["summary"]=json!("The application stopped before this task finished. Review its changes, then continue.");
            let mut verification = match job["task_id"].as_str() {
                Some(tid) => task_verification_on(&tx, tid)?,
                None => json!({"status":"incomplete","commands":[]}),
            };
            verification["verified"] = json!(false);
            if verification["claim"] == "verified" {
                verification["claim"] = json!("observed");
            }
            verification["status"] = json!("incomplete");
            verification["red_green"] = json!(false);
            verification["final_assessment"] = json!("interrupted");
            let result = json!({
                "success": false,
                "cancelled": false,
                "interrupted": true,
                "summary": job["summary"],
                "verification": verification,
                "usage": job.get("usage").cloned().unwrap_or(json!({}))
            });
            job["result"] = result.clone();
            if let Some(tid) = job["task_id"].as_str() {
                let sid = finish_task_on(
                    &tx,
                    tid,
                    "interrupted",
                    job["summary"].as_str().unwrap_or("Task interrupted"),
                    &job["usage"],
                )?;
                let ts = now();
                tx.execute(
                    "INSERT INTO events(ts,type,session_id,task_id,payload) VALUES(?,'agent.completed',?,?,?)",
                    params![ts, sid, tid, result.to_string()],
                )?;
                job["event_cursor"] = json!(tx.last_insert_rowid());
            }
            tx.execute(
                "UPDATE desktop_jobs SET payload=? WHERE id=?",
                params![job.to_string(), job["id"].as_str()],
            )?;
        }
        tx.commit()?;
        Ok(jobs.len())
    }
    pub fn save_messages(&self, job_id: &str, messages: &[Value]) -> Result<()> {
        let payloads: Vec<String> = messages.iter().map(Value::to_string).collect();
        self.save_message_payloads(job_id, &payloads)
    }
    /// Replace a job's message tape with already serialised messages.
    pub fn save_message_payloads(&self, job_id: &str, payloads: &[String]) -> Result<()> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        tx.execute("DELETE FROM job_messages WHERE job_id=?", [job_id])?;
        for (ordinal, payload) in payloads.iter().enumerate() {
            tx.execute(
                "INSERT INTO job_messages(job_id,ordinal,payload) VALUES(?,?,?)",
                params![job_id, ordinal, payload],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn messages(&self, job_id: &str) -> Result<Vec<Value>> {
        Ok(self
            .query(
                "SELECT payload FROM job_messages WHERE job_id=? ORDER BY ordinal",
                [job_id],
            )?
            .into_iter()
            .map(|v| v["payload"].clone())
            .collect())
    }
    pub fn touch_project(&self, path: &Path) -> Result<()> {
        let path = path.canonicalize()?;
        self.execute("INSERT INTO projects(id,path,name,last_opened) VALUES(?,?,?,?) ON CONFLICT(path) DO UPDATE SET last_opened=excluded.last_opened",params![id(),path.to_string_lossy(),path.file_name().map(|s|s.to_string_lossy().into_owned()).unwrap_or_else(||"/".into()),now()])?;
        Ok(())
    }
    pub fn projects(&self) -> Result<Vec<Value>> {
        self.query(
            "SELECT * FROM projects ORDER BY last_opened DESC LIMIT 1000",
            [],
        )
    }
    pub fn models(&self) -> Result<Vec<Value>> {
        self.query("SELECT * FROM models ORDER BY name", [])
    }
    pub fn upsert_model(&self, model: &Value) -> Result<()> {
        self.execute("INSERT INTO models(id,name,provider,endpoint,context_limit,metadata) VALUES(?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,provider=excluded.provider,endpoint=excluded.endpoint,context_limit=excluded.context_limit,metadata=excluded.metadata",
            params![model["id"].as_str().context("Model ID required")?,model["name"].as_str().context("Model name required")?,model["provider"].as_str().context("Provider required")?,model["endpoint"].as_str().unwrap_or(""),model["context_limit"].as_u64().unwrap_or(128000),model.get("metadata").unwrap_or(&json!({})).to_string()])?;
        Ok(())
    }
    pub fn upsert_detected_model(&self, model: &Value) -> Result<()> {
        self.execute("INSERT INTO models(id,name,provider,endpoint,context_limit,metadata) VALUES(?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET context_limit=CASE WHEN json_extract(models.metadata,'$.api_key_env') IS NULL THEN excluded.context_limit ELSE models.context_limit END,metadata=json_patch(models.metadata,excluded.metadata) WHERE models.name=excluded.name AND models.provider=excluded.provider AND models.endpoint=excluded.endpoint",
            params![model["id"].as_str().context("Model ID required")?,model["name"].as_str().context("Model name required")?,model["provider"].as_str().context("Provider required")?,model["endpoint"].as_str().context("Endpoint required")?,model["context_limit"].as_u64().context("Context limit required")?,model["metadata"].to_string()])?;
        Ok(())
    }
    pub fn pins(&self, sid: &str) -> Result<Vec<Value>> {
        self.query("SELECT * FROM pins WHERE session_id=? ORDER BY id", [sid])
    }
    pub fn add_pin(&self, sid: &str, label: &str, body: &str) -> Result<i64> {
        ensure!(self.session(sid)?.is_some(), "Session not found");
        let db = self.lock()?;
        db.execute(
            "INSERT INTO pins(session_id,ts,label,body) VALUES(?,?,?,?)",
            params![sid, now(), label, body],
        )?;
        Ok(db.last_insert_rowid())
    }
    pub fn delete_pin(&self, sid: &str, id: i64) -> Result<()> {
        self.execute(
            "DELETE FROM pins WHERE session_id=? AND id=?",
            params![sid, id],
        )?;
        Ok(())
    }
}

/// Shared by ordinary finalization and startup recovery (inside its existing
/// transaction). Preserve a completed assessment verbatim unless a newer
/// observed receipt or native inspection proves it was not the task's final
/// evidence snapshot.
fn task_verification_on(db: &Connection, tid: &str) -> Result<Value> {
    let previous = query_rows(
        db,
        "SELECT id,payload FROM events WHERE task_id=? AND type='verification.summary' ORDER BY id DESC LIMIT 1",
        [tid],
    )?
    .pop()
    .filter(|event| event["payload"].is_object() && event["payload"]["commands"].is_array());
    let after = previous
        .as_ref()
        .and_then(|event| event["id"].as_i64())
        .unwrap_or(0);
    let receipts: Vec<Value> = query_rows(
        db,
        "SELECT payload FROM events WHERE task_id=? AND type='verification.receipt' AND id>? ORDER BY id",
        params![tid, after],
    )?
    .into_iter()
    .map(|event| event["payload"].clone())
    .filter(|receipt| receipt.is_object() && receipt["task_id"] == tid)
    .collect();
    // Both native and vendor tasks emit tool.completed. The host-generated
    // start event identifies the execution owner; provider-controlled tool
    // names must not become native inspection evidence on a failed vendor turn.
    let native_start = query_rows(
        db,
        "SELECT id,json_type(payload,'$.native') AS native_type FROM events WHERE task_id=? AND type='agent.started' ORDER BY id DESC LIMIT 1",
        [tid],
    )?
    .pop()
    .filter(|event| event["native_type"] == "true")
    .and_then(|event| event["id"].as_i64());
    let mut observed_workspace = false;
    let mut observed_host = false;
    let mut newer_observation = false;
    if let Some(native_start) = native_start {
        // Project only known names and their latest event IDs, never arguments
        // or output. The result is bounded by the shared inspection allowlist.
        let tools = crate::verification::INSPECTION_TOOLS;
        let placeholders = vec!["?"; tools.len()].join(",");
        let mut arguments = vec![
            rusqlite::types::Value::from(tid.to_owned()),
            rusqlite::types::Value::from(native_start),
        ];
        arguments.extend(
            tools
                .iter()
                .map(|(tool, _)| rusqlite::types::Value::from((*tool).to_owned())),
        );
        let observations = query_rows(
            db,
            &format!(
                "SELECT json_extract(payload,'$.tool') AS tool,MAX(id) AS id FROM events WHERE task_id=? AND id>? AND type='tool.completed' AND json_type(payload,'$.success')='true' AND json_type(payload,'$.tool')='text' AND json_extract(payload,'$.tool') IN ({placeholders}) GROUP BY json_extract(payload,'$.tool')"
            ),
            rusqlite::params_from_iter(arguments),
        )?;
        for event in observations {
            match event["tool"]
                .as_str()
                .and_then(crate::verification::inspection_scope)
            {
                Some(crate::verification::InspectionScope::Workspace) => observed_workspace = true,
                Some(crate::verification::InspectionScope::Host) => observed_host = true,
                None => continue,
            }
            newer_observation |= event["id"].as_i64().is_some_and(|id| id > after);
        }
    }
    let needs_assessment = previous.is_none() || !receipts.is_empty() || newer_observation;
    let mut summary = previous
        .map(|event| event["payload"].clone())
        .unwrap_or_else(|| json!({"commands":[]}));
    if needs_assessment {
        let inspected_workspace = observed_workspace || summary["inspected_workspace"] == true;
        let inspected_host = observed_host || summary["inspected_host"] == true;
        let mut commands = summary["commands"].as_array().cloned().unwrap_or_default();
        commands.extend(receipts);
        let assessed = crate::verification::classify_observations(
            "",
            &commands,
            inspected_workspace,
            inspected_host,
        );
        summary
            .as_object_mut()
            .expect("verification summary is an object")
            .extend(
                assessed
                    .as_object()
                    .expect("classification is an object")
                    .clone(),
            );
        // Successful individual commands describe their observed snapshots;
        // they cannot substitute for the final assessment that did not run.
        summary["verified"] = json!(false);
        summary["claim"] = json!(
            if !commands.is_empty() || inspected_workspace || inspected_host {
                "observed"
            } else {
                "model_claim"
            }
        );
        summary["status"] = json!("incomplete");
        summary["red_green"] = json!(false);
        summary["final_assessment"] = json!("not_completed");
        summary["note"] = json!("Recorded command receipts are retained, but final verification assessment did not complete. Current file freshness has not been reassessed.");
    }
    Ok(summary)
}

fn query_rows(db: &Connection, sql: &str, args: impl Params) -> Result<Vec<Value>> {
    let mut statement = db.prepare(sql)?;
    let columns: Vec<String> = statement
        .column_names()
        .iter()
        .map(|s| (*s).into())
        .collect();
    let rows = statement
        .query_map(args, |row| {
            let mut out = Map::new();
            for (i, key) in columns.iter().enumerate() {
                let value = match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => json!(v),
                    ValueRef::Real(v) => json!(v),
                    ValueRef::Text(v) => {
                        let text = String::from_utf8_lossy(v).into_owned();
                        if matches!(key.as_str(), "payload" | "metadata") {
                            serde_json::from_str(&text).unwrap_or(Value::Null)
                        } else {
                            json!(text)
                        }
                    }
                    ValueRef::Blob(_) => Value::Null,
                };
                out.insert(key.clone(), value);
            }
            Ok(Value::Object(out))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn finish_task_on(
    tx: &Connection,
    tid: &str,
    status: &str,
    summary: &str,
    usage: &Value,
) -> Result<String> {
    let sid: String = tx.query_row("SELECT session_id FROM tasks WHERE id=?", [tid], |r| {
        r.get(0)
    })?;
    let previous: Option<String> =
        tx.query_row("SELECT usage_json FROM tasks WHERE id=?", [tid], |r| {
            r.get(0)
        })?;
    let current: Option<String> =
        tx.query_row("SELECT usage_json FROM sessions WHERE id=?", [&sid], |r| {
            r.get(0)
        })?;
    // Re-finishing a task replaces its earlier contribution.
    let mut total = crate::usage::parse(&json!(current));
    total.subtract(&crate::usage::parse(&json!(previous)));
    total.add(&crate::usage::parse(usage));
    let total = json!(total);
    tx.execute(
        "UPDATE tasks SET status=?,summary=?,completed_at=?,usage_json=? WHERE id=?",
        params![status, summary, now(), usage.to_string(), tid],
    )?;
    tx.execute(
        "UPDATE sessions SET updated_at=?,usage_json=? WHERE id=?",
        params![now(), total.to_string(), sid],
    )?;
    Ok(sid)
}

#[cfg(test)]
mod verification_tests {
    use super::*;

    #[test]
    fn terminal_workspace_inspections_require_owned_successful_allowlisted_events() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("store.sqlite");
        let store = Store::open(&path).unwrap();
        // Independent behavior inventory: never derive expectations from the
        // production allowlist, so accidentally broadening it cannot pass.
        let workspace_tools = [
            "read_file",
            "search_text",
            "search_symbol",
            "workspace_symbols",
            "goto_definition",
            "find_references",
            "get_diagnostics",
            "get_type_signature",
            "repo_map",
            "search_code",
            "git_diff",
            "git_status",
            "git_log",
            "mcp_sqlite_tables",
            "mcp_sqlite_query",
            "background_list",
            "background_output",
        ];
        for tool in workspace_tools {
            store
                .add_event("agent.started", &json!({"native":true}), None, Some(tool))
                .unwrap();
            store
                .add_event(
                    "tool.completed",
                    &json!({"tool":tool,"success":true,"task_id":"forged-payload-owner"}),
                    Some("shared-session"),
                    Some(tool),
                )
                .unwrap();
        }
        for (task, kind, tool, success) in [
            ("failed", "tool.completed", json!("read_file"), json!(false)),
            ("numeric", "tool.completed", json!("read_file"), json!(1)),
            (
                "string",
                "tool.completed",
                json!("read_file"),
                json!("true"),
            ),
            ("missing", "tool.completed", json!("read_file"), Value::Null),
            ("started", "tool.started", json!("read_file"), json!(true)),
            ("exec", "tool.completed", json!("exec"), json!(true)),
            ("edit", "tool.completed", json!("edit_file"), json!(true)),
            (
                "unknown",
                "tool.completed",
                json!("vendor_read_file"),
                json!(true),
            ),
            (
                "nested-tool",
                "tool.completed",
                json!({"name":"read_file"}),
                json!(true),
            ),
            (
                "host-only",
                "tool.completed",
                json!("system_info"),
                json!(true),
            ),
        ] {
            store
                .add_event("agent.started", &json!({"native":true}), None, Some(task))
                .unwrap();
            store
                .add_event(
                    kind,
                    &json!({"tool":tool,"success":success}),
                    Some("shared-session"),
                    Some(task),
                )
                .unwrap();
        }
        for task in ["session-only", "forged-payload-owner", "unrelated"] {
            store
                .add_event("agent.started", &json!({"native":true}), None, Some(task))
                .unwrap();
        }
        store
            .add_event(
                "tool.completed",
                &json!({"tool":"read_file","success":true,
            "task_id":"session-only"}),
                Some("shared-session"),
                None,
            )
            .unwrap();
        for tool in workspace_tools {
            let summary = store.task_verification(tool).unwrap();
            assert_eq!(summary["inspected_workspace"], true, "{tool}: {summary}");
            assert_eq!(summary["inspected_host"], false, "{tool}: {summary}");
            assert_eq!(summary["claim"], "observed");
            assert_eq!(summary["verified"], false);
            assert_eq!(summary["red_green"], false);
            assert_eq!(summary["status"], "incomplete");
            assert_eq!(summary["final_assessment"], "not_completed");
            assert_eq!(summary["commands"], json!([]));
        }
        for task in [
            "failed",
            "numeric",
            "string",
            "missing",
            "started",
            "exec",
            "edit",
            "unknown",
            "nested-tool",
            "host-only",
            "session-only",
            "forged-payload-owner",
            "unrelated",
        ] {
            let summary = store.task_verification(task).unwrap();
            assert_eq!(summary["inspected_workspace"], false, "{task}: {summary}");
            assert_eq!(summary["inspected_host"], task == "host-only");
            assert_eq!(summary["verified"], false);
            assert_eq!(
                summary["claim"],
                if task == "host-only" {
                    "observed"
                } else {
                    "model_claim"
                }
            );
        }
        let expected = store.task_verification("read_file").unwrap();
        drop(store);
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .task_verification("read_file")
                .unwrap(),
            expected
        );
    }

    #[test]
    fn terminal_workspace_history_preserves_later_assessment_and_retains_receipts() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("store.sqlite")).unwrap();
        store
            .add_event("agent.started", &json!({"native":true}), None, Some("task"))
            .unwrap();
        let observation = json!({"tool":"read_file","success":true});
        store
            .add_event("tool.completed", &observation, None, Some("task"))
            .unwrap();
        let command = receipt("task", "check");
        let mut summary = crate::verification::classify_observations(
            "",
            std::slice::from_ref(&command),
            false,
            true,
        );
        summary["custom"] = json!("preserve");
        store
            .add_event("verification.summary", &summary, None, Some("task"))
            .unwrap();
        // A newer assessment takes precedence over historical inspection events.
        assert_eq!(store.task_verification("task").unwrap(), summary);
        store
            .add_event("tool.completed", &observation, None, Some("task"))
            .unwrap();
        let after = store.task_verification("task").unwrap();
        assert_eq!(after["inspected_workspace"], true);
        assert_eq!(after["inspected_host"], true);
        assert_eq!(after["custom"], "preserve");
        assert_eq!(after["commands"], json!([command]));
        assert_eq!(after["commands"][0]["state"], "passed");
        assert_eq!(after["status"], "incomplete");
        assert_eq!(after["final_assessment"], "not_completed");
        assert_eq!(after["claim"], "observed");
        assert_eq!(after["verified"], false);
        assert_eq!(after["red_green"], false);
        assert_eq!(store.task_verification("task").unwrap(), after);
    }

    #[test]
    fn interrupted_workspace_inspection_recovery_is_durable_and_task_scoped() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("store.sqlite");
        let store = Store::open(&path).unwrap();
        let session = store.create_session(root.path(), "fixture", "").unwrap();
        let sid = session["id"].as_str().unwrap();
        let first = store.create_task(sid, "Observed task").unwrap();
        let second = store.create_task(sid, "Unobserved task").unwrap();
        for (id, task) in [("observed-job", &first), ("unobserved-job", &second)] {
            store
                .save_job(&json!({"id":id,"session_id":sid,"task_id":task,"status":"running"}))
                .unwrap();
            store
                .add_event(
                    "agent.started",
                    &json!({"native":true}),
                    Some(sid),
                    Some(task),
                )
                .unwrap();
        }
        store
            .add_event(
                "tool.completed",
                &json!({"tool":"search_text","success":true,
            "task_id":second}),
                Some(sid),
                Some(&first),
            )
            .unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.recover_jobs().unwrap(), 2);
        assert_eq!(reopened.recover_jobs().unwrap(), 0);
        for (id, inspected) in [("observed-job", true), ("unobserved-job", false)] {
            let job = reopened.job(id).unwrap().unwrap();
            let v = &job["result"]["verification"];
            assert_eq!(job["status"], "interrupted");
            assert_eq!(v["inspected_workspace"], inspected, "{id}: {v}");
            assert_eq!(v["inspected_host"], false);
            assert_eq!(v["verified"], false);
            assert_eq!(v["red_green"], false);
            assert_eq!(v["status"], "incomplete");
            assert_eq!(v["final_assessment"], "interrupted");
            assert_eq!(
                v["claim"],
                if inspected { "observed" } else { "model_claim" }
            );
        }
    }

    #[test]
    fn terminal_inspections_require_native_origin_before_the_observation() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("store.sqlite")).unwrap();
        for tool in ["read_file", "system_info"] {
            for (kind, marker) in [
                ("native", Some(json!(true))),
                ("vendor", Some(json!(false))),
                ("numeric", Some(json!(1))),
                ("string", Some(json!("true"))),
                ("missing", None),
                ("before-start", None),
                ("latest-vendor", Some(json!(true))),
            ] {
                let task = format!("{tool}-{kind}");
                if let Some(native) = marker {
                    store
                        .add_event(
                            "agent.started",
                            &json!({"native":native}),
                            None,
                            Some(&task),
                        )
                        .unwrap();
                }
                if kind == "latest-vendor" {
                    store
                        .add_event("agent.started", &json!({"native":false}), None, Some(&task))
                        .unwrap();
                }
                // Copied native/task claims inside a provider tool payload are not authority.
                store
                    .add_event(
                        "tool.completed",
                        &json!({"tool":tool,"success":true,
                    "native":true,"task_id":"native"}),
                        None,
                        Some(&task),
                    )
                    .unwrap();
                if kind == "before-start" {
                    store
                        .add_event("agent.started", &json!({"native":true}), None, Some(&task))
                        .unwrap();
                }
                let v = store.task_verification(&task).unwrap();
                assert_eq!(
                    v["inspected_workspace"],
                    kind == "native" && tool == "read_file",
                    "{task}: {v}"
                );
                assert_eq!(
                    v["inspected_host"],
                    kind == "native" && tool == "system_info",
                    "{task}: {v}"
                );
                assert_eq!(v["verified"], false);
                assert_eq!(
                    v["claim"],
                    if kind == "native" {
                        "observed"
                    } else {
                        "model_claim"
                    }
                );
            }
        }
    }

    #[test]
    fn terminal_verification_preserves_only_owned_successful_host_observations() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("store.sqlite");
        let store = Store::open(&path).unwrap();
        for (task, tool, success) in [
            ("host", "system_info", json!(true)),
            ("failed", "system_info", json!(false)),
            ("numeric", "system_info", json!(1)),
            ("other-tool", "edit_file", json!(true)),
        ] {
            store
                .add_event("agent.started", &json!({"native":true}), None, Some(task))
                .unwrap();
            store
                .add_event(
                    "tool.completed",
                    &json!({"tool":tool,"success":success,
                "task_id":"wrong-payload-owner"}),
                    None,
                    Some(task),
                )
                .unwrap();
        }
        for task in [
            "host",
            "failed",
            "numeric",
            "other-tool",
            "wrong-payload-owner",
            "missing",
        ] {
            let summary = store.task_verification(task).unwrap();
            assert_eq!(
                summary["inspected_host"],
                task == "host",
                "{task}: {summary}"
            );
            assert_eq!(summary["inspected_workspace"], false, "{task}: {summary}");
            assert_eq!(
                summary["claim"],
                if task == "host" {
                    "observed"
                } else {
                    "model_claim"
                }
            );
            assert_eq!(summary["verified"], false);
            assert_eq!(summary["status"], "incomplete");
        }
        let expected = store.task_verification("host").unwrap();
        drop(store);
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .task_verification("host")
                .unwrap(),
            expected
        );
    }

    #[test]
    fn terminal_host_history_does_not_replace_a_later_assessment_but_new_evidence_does() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("store.sqlite")).unwrap();
        store
            .add_event("agent.started", &json!({"native":true}), None, Some("task"))
            .unwrap();
        let observation = json!({"tool":"system_info","success":true});
        store
            .add_event("tool.completed", &observation, None, Some("task"))
            .unwrap();
        // Legacy summary: retained exactly, not retroactively reclassified
        // because the modern separate host scope was absent.
        let summary = json!({"status":"not_run","commands":[],"inspected_workspace":true,
            "claim":"observed","verified":false,"custom":"retain"});
        store
            .add_event("verification.summary", &summary, None, Some("task"))
            .unwrap();
        assert_eq!(store.task_verification("task").unwrap(), summary);
        store
            .add_event("tool.completed", &observation, None, Some("task"))
            .unwrap();
        let after = store.task_verification("task").unwrap();
        assert_eq!(after["inspected_host"], true);
        assert_eq!(after["inspected_workspace"], true);
        assert_eq!(after["custom"], "retain");
        assert_eq!(after["status"], "incomplete");
        assert_eq!(after["final_assessment"], "not_completed");
        assert_eq!(after["verified"], false);
    }

    fn receipt(task: &str, call: &str) -> Value {
        json!({
            "schema_version":1,"task_id":task,"attempt_id":"attempt",
            "tool_call_id":call,"check_id":"check","workspace":"/project",
            "cwd":"/project","command":"test-command","kind":"configured_check",
            "state":"passed","provenance":"locally_observed","scope":"fixture",
            "started_at":1.0,"finished_at":2.0,"process_seconds":1.0,
            "exit_code":0,"termination_reason":"exited","workspace_fingerprint":"hash",
            "output_ref":format!("tool.completed:{call}"),"success":true,"timed_out":false
        })
    }

    #[test]
    fn terminal_verification_preserves_an_assessed_stale_summary() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("store.sqlite");
        let store = Store::open(&path).unwrap();
        let raw = receipt("task", "call-1");
        store
            .add_event("verification.receipt", &raw, None, Some("task"))
            .unwrap();
        let mut stale = raw;
        stale["state"] = json!("stale");
        stale["success"] = json!(false);
        let mut summary = crate::verification::classify("Done", &[stale], true);
        summary["hooks"] = json!({"on_complete":"observed"});
        store
            .add_event("verification.summary", &summary, None, Some("task"))
            .unwrap();
        assert_eq!(store.task_verification("task").unwrap(), summary);
        drop(store);
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .task_verification("task")
                .unwrap(),
            summary
        );
    }

    #[test]
    fn terminal_verification_appends_only_new_receipts_without_reviving_stale_evidence() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("store.sqlite")).unwrap();
        let raw = receipt("task", "call-1");
        store
            .add_event("verification.receipt", &raw, None, Some("task"))
            .unwrap();
        let mut stale = raw;
        stale["state"] = json!("stale");
        stale["success"] = json!(false);
        let mut summary = crate::verification::classify("Done", &[stale.clone()], true);
        summary["hooks"] = json!({"on_complete":"observed"});
        store
            .add_event("verification.summary", &summary, None, Some("task"))
            .unwrap();
        let latest = receipt("task", "call-2");
        store
            .add_event("verification.receipt", &latest, None, Some("task"))
            .unwrap();
        let result = store.task_verification("task").unwrap();
        assert_eq!(result["commands"], json!([stale, latest]));
        assert_eq!(result["hooks"], summary["hooks"]);
        assert_eq!(result["verified"], false);
        assert_eq!(result["red_green"], false);
        assert_eq!(result["claim"], "observed");
        assert_eq!(result["status"], "incomplete");
        assert_eq!(result["final_assessment"], "not_completed");
        // Finalization is read-only and repeated snapshots cannot duplicate receipts.
        assert_eq!(store.task_verification("task").unwrap(), result);
    }

    #[test]
    fn terminal_verification_ignores_misfiled_receipts_and_unrelated_task_events() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("store.sqlite")).unwrap();
        for event_task in ["task", "other"] {
            store
                .add_event(
                    "verification.receipt",
                    &receipt("other", "foreign"),
                    None,
                    Some(event_task),
                )
                .unwrap();
        }
        store
            .add_event(
                "tool.completed",
                &receipt("task", "untyped"),
                None,
                Some("task"),
            )
            .unwrap();
        let result = store.task_verification("task").unwrap();
        assert_eq!(result["commands"], json!([]));
        assert_eq!(result["verified"], false);
        assert_eq!(result["claim"], "model_claim");
        assert_eq!(result["final_assessment"], "not_completed");
    }
}

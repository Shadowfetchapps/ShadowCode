//! On-demand code index in ordinary SQLite tables: tree-sitter definitions
//! (`symbols`), identifier counts per file (`refs`, the def/ref graph behind
//! the repo map) and FTS5 code chunks (`chunks`, `chunks_fts`) for BM25
//! search. Languages: Rust, TypeScript/TSX, JavaScript, Python, Go, C, C++ and
//! Java; other text files are chunked for search only.
//!
//! Files are indexed when touched or during a project scan — never the whole
//! world at startup. A scan walks every indexable file (up to 250,000) but
//! parses at most 2,000 changed ones per call, so a large repository fills in
//! over a few calls while each stays short; unchanged files (same mtime and
//! size) are skipped without being read. The app and the CLI keep the
//! database in the profile's cache (`use_cache`), so the index survives
//! restarts; an index from another version, or a damaged one, is rebuilt.
//! Without `use_cache` (embedders, tests) it lives in a private per-process
//! folder. It is never stored in the project. A project may set a focus
//! folder: only that subtree is scanned.
use crate::code_intel::{
    chunks,
    langs::{self, Lang},
};
use crate::workspace::Workspace;
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

/// Most changed files one call parses (unchanged ones are only checked).
const PARSE_BUDGET: usize = 2_000;
/// Most indexable files a project scan follows.
const MAX_INDEX_FILES: usize = 250_000;
const MAX_WALK_ENTRIES: usize = 1_000_000;
/// Bumped when the tables change: an index from another version is rebuilt.
const INDEX_VERSION: &str = "2";
const MAX_FILE_BYTES: usize = 512_000;
const MAX_CALLERS: usize = 24;
const MAX_REFS_PER_FILE: usize = 4_000;
/// A repeated scan of the same project inside this window reuses the last one;
/// edits made through the native tools are re-indexed by `touch` anyway.
const RESCAN_AFTER: Duration = Duration::from_secs(2);
/// The same for a project with more files than a scan follows: walking it
/// again is the slow part, and cannot make the index cover more of it.
const RESCAN_CAPPED_AFTER: Duration = Duration::from_secs(60);
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS files (
  path TEXT PRIMARY KEY,
  lang TEXT NOT NULL,
  mtime_ns INTEGER NOT NULL,
  size INTEGER NOT NULL,
  digest TEXT NOT NULL,
  indexed_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS symbols (
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  line INTEGER NOT NULL,
  column INTEGER NOT NULL,
  signature TEXT NOT NULL,
  FOREIGN KEY(path) REFERENCES files(path) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_path ON symbols(path);
CREATE TABLE IF NOT EXISTS refs (
  path TEXT NOT NULL,
  name TEXT NOT NULL,
  count INTEGER NOT NULL,
  PRIMARY KEY(path, name),
  FOREIGN KEY(path) REFERENCES files(path) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_refs_name ON refs(name);
"#;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn mtime_ns(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// Keep derived data out of the project (including read-only reviews). The
// private process cache cannot be redirected by a project's .shadow symlink.
static CACHE: Mutex<Option<tempfile::TempDir>> = Mutex::new(None);
/// The profile's index folder, when the app or CLI set one.
static CACHE_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Keep indexes in `dir` (the profile's cache) from now on.
pub fn use_cache(dir: PathBuf) {
    if let Ok(mut current) = CACHE_DIR.lock() {
        *current = Some(dir);
    }
}

fn db_path(root: &Path) -> Result<PathBuf> {
    db_path_for(&root.canonicalize()?)
}
/// The index file of a project by its canonical path (which may be gone).
fn db_path_for(root: &Path) -> Result<PathBuf> {
    let key = format!("{:x}", Sha256::digest(root.as_os_str().as_encoded_bytes()));
    if let Some(dir) = CACHE_DIR.lock().ok().and_then(|d| d.clone()) {
        crate::paths::private_directory(&dir)?;
        return Ok(dir.join(format!("{key}.sqlite")));
    }
    let mut cache = CACHE
        .lock()
        .map_err(|_| anyhow::anyhow!("Index cache lock poisoned"))?;
    if cache.is_none() {
        *cache = Some(
            tempfile::Builder::new()
                .prefix("shadowcode-symbols-")
                .tempdir()?,
        );
    }
    Ok(cache
        .as_ref()
        .context("Index cache unavailable")?
        .path()
        .join(format!("{key}.sqlite")))
}
fn open_db(root: &Path) -> Result<Connection> {
    open_or_rebuild(&db_path(root)?)
}

/// Open an index file; a damaged one, or one from another version, is
/// deleted and started again (the index is rebuildable).
fn open_or_rebuild(path: &Path) -> Result<Connection> {
    match open_db_at(path) {
        Ok(conn) => Ok(conn),
        Err(_) => {
            remove_db(path);
            open_db_at(path)
        }
    }
}

fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let _ = fs::remove_file(PathBuf::from(name));
    }
}

fn open_db_at(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
    )?;
    conn.execute_batch(SCHEMA)?;
    conn.execute_batch(chunks::SCHEMA)?;
    let version: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='version'", [], |r| {
            r.get(0)
        })
        .ok();
    match version.as_deref() {
        Some(INDEX_VERSION) => {}
        None => {
            let files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
            anyhow::ensure!(files == 0, "index from another version");
            conn.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES('version',?)",
                [INDEX_VERSION],
            )?;
        }
        Some(_) => anyhow::bail!("index from another version"),
    }
    Ok(conn)
}

fn meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0))
        .ok()
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES(?,?)",
        [key, value],
    )?;
    Ok(())
}

/// Scan only `focus` (a folder of the project) from now on; `None` scans
/// the whole project. Files outside it leave the index at the next full
/// scan.
pub fn set_focus(root: &Path, focus: Option<&str>) -> Result<Value> {
    let conn = open_db(root)?;
    match focus.map(str::trim).filter(|f| !f.is_empty() && *f != ".") {
        Some(folder) => {
            let workspace = Workspace::open(root)?;
            let rel = workspace.relative(folder)?;
            ensure!(root.join(&rel).is_dir(), "Choose a folder in the project");
            set_meta(&conn, "focus", &rel.to_string_lossy())?;
        }
        None => {
            conn.execute("DELETE FROM meta WHERE key='focus'", [])?;
        }
    }
    forget_recent_scan(root);
    drop(conn);
    stats(root)
}

/// Delete this project's index (it is rebuilt when needed). A folder that is
/// already gone (a removed worktree) is named by the canonical path it had.
pub fn clear(root: &Path) -> Result<()> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    remove_db(&db_path_for(&root)?);
    if let Ok(mut scans) = LAST_SCAN.lock() {
        if let Some(scans) = scans.as_mut() {
            scans.remove(&root);
        }
    }
    Ok(())
}

/// Where the index of a project (by its canonical path) is kept.
#[cfg(test)]
pub(crate) fn index_path(root: &Path) -> PathBuf {
    db_path_for(root).unwrap()
}

fn forget_recent_scan(root: &Path) {
    if let (Ok(key), Ok(mut scans)) = (root.canonicalize(), LAST_SCAN.lock()) {
        if let Some(scans) = scans.as_mut() {
            scans.remove(&key);
        }
    }
}

/// Read-only access for search and the repo map. Call `ensure_index` first.
pub(crate) fn open_index(root: &Path) -> Result<Connection> {
    open_db(root)
}

fn read_source(root: &Path, path: &Path) -> Result<String> {
    let workspace = Workspace::open(root)?;
    let rel = workspace.relative(path.to_str().context("Source path is not UTF-8")?)?;
    ensure!(
        !crate::redaction::is_secret_path(&rel.to_string_lossy()),
        "Secret paths are not indexed"
    );
    let file = workspace.read(&rel.to_string_lossy())?;
    ensure!(
        file.bytes <= MAX_FILE_BYTES,
        "Source exceeds the AST index byte limit"
    );
    Ok(file.content)
}

/// The node that stands for the whole definition of a captured name.
fn definition_node<'t>(lang: Lang, name: tree_sitter::Node<'t>) -> tree_sitter::Node<'t> {
    let kinds = langs::definition_kinds(lang);
    let parent = name.parent().unwrap_or(name);
    if kinds.is_empty() {
        return parent;
    }
    let mut node = name;
    for _ in 0..6 {
        let Some(next) = node.parent() else {
            break;
        };
        if kinds.contains(&next.kind()) {
            return next;
        }
        node = next;
    }
    parent
}

fn extract_signature(source: &str, node: tree_sitter::Node) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    let slice = &source[start..end];
    let first = slice.lines().next().unwrap_or(slice).trim();
    let mut out = first.to_owned();
    if out.len() > 240 {
        let mut end = 240;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('…');
    }
    out
}

struct Grammar {
    parser: Parser,
    definitions: Query,
    references: Query,
}

/// Parsers and compiled queries, built once per indexing pass.
#[derive(Default)]
struct Grammars(HashMap<Lang, Grammar>);
impl Grammars {
    fn get(&mut self, lang: Lang) -> Result<&mut Grammar> {
        use std::collections::hash_map::Entry;
        Ok(match self.0.entry(lang) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let language = langs::language(lang);
                let mut parser = Parser::new();
                parser.set_language(&language)?;
                entry.insert(Grammar {
                    parser,
                    definitions: Query::new(&language, &langs::definition_query(lang))?,
                    references: Query::new(&language, langs::reference_query(lang))?,
                })
            }
        })
    }
}

type ParsedSymbol = (String, String, usize, usize, String);
type Parsed = (Vec<ParsedSymbol>, HashMap<String, i64>);

fn parse_file(grammar: &mut Grammar, source: &str, lang: Lang) -> Parsed {
    let Some(tree) = grammar.parser.parse(source, None) else {
        return Default::default();
    };
    let mut symbols = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&grammar.definitions, tree.root_node(), source.as_bytes());
    while let Some(m) = matches.next() {
        for capture in m.captures {
            let name = capture
                .node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .to_owned();
            if name.is_empty() {
                continue;
            }
            let definition = definition_node(lang, capture.node);
            symbols.push((
                name,
                definition.kind().to_owned(),
                capture.node.start_position().row + 1,
                capture.node.start_position().column + 1,
                extract_signature(source, definition),
            ));
        }
    }
    let mut refs: HashMap<String, i64> = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&grammar.references, tree.root_node(), source.as_bytes());
    while let Some(m) = matches.next() {
        for capture in m.captures {
            let name = capture.node.utf8_text(source.as_bytes()).unwrap_or("");
            if name.len() < 2 || name.len() > 80 {
                continue;
            }
            if let Some(count) = refs.get_mut(name) {
                *count += 1;
            } else if refs.len() < MAX_REFS_PER_FILE {
                refs.insert(name.to_owned(), 1);
            }
        }
    }
    (symbols, refs)
}

/// Walk indexable files. Returns the files and whether the walk saw the whole
/// project (no entry or file limit reached).
/// Indexable files below `start` (the project or its focus folder).
fn walk_sources(start: &Path, limit: usize) -> (Vec<PathBuf>, bool) {
    let mut walker = ignore::WalkBuilder::new(start)
        .follow_links(false)
        .max_depth(Some(32))
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.depth() == 0
                || (!name.starts_with('.')
                    && !matches!(
                        name.as_ref(),
                        "node_modules" | "target" | "dist" | "build" | "__pycache__" | "vendor"
                    )
                    && !crate::redaction::is_secret_path(&entry.path().to_string_lossy()))
        })
        .build();
    let mut out = Vec::new();
    let mut entries = 0usize;
    for entry in walker.by_ref() {
        entries += 1;
        if entries > MAX_WALK_ENTRIES {
            return (out, false);
        }
        let Ok(entry) = entry else {
            continue;
        };
        if entry.file_type().is_some_and(|kind| kind.is_file())
            && langs::indexable(entry.path())
            && entry
                .metadata()
                .is_ok_and(|metadata| metadata.len() <= MAX_FILE_BYTES as u64)
        {
            if out.len() >= limit {
                return (out, false);
            }
            out.push(entry.into_path());
        }
    }
    (out, true)
}

/// True when the project has at least one file a grammar can parse (stops at
/// the first one; bounded like a scan).
pub fn has_sources(root: &Path) -> bool {
    ignore::WalkBuilder::new(root)
        .follow_links(false)
        .max_depth(Some(32))
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.depth() == 0
                || (!name.starts_with('.')
                    && !matches!(
                        name.as_ref(),
                        "node_modules" | "target" | "dist" | "build" | "vendor"
                    ))
        })
        .build()
        .take(MAX_WALK_ENTRIES)
        .filter_map(|entry| entry.ok())
        .any(|entry| {
            entry.file_type().is_some_and(|kind| kind.is_file())
                && langs::lang_for(entry.path()).is_some()
        })
}

enum Indexed {
    Skipped,
    Unchanged,
    Updated(usize),
}

fn index_file(
    conn: &Connection,
    root: &Path,
    path: &Path,
    grammars: &mut Grammars,
    force: bool,
) -> Result<Indexed> {
    let lang = langs::lang_for(path);
    if lang.is_none() && !langs::searchable_text(path) {
        return Ok(Indexed::Skipped);
    }
    let rel = relative(root, path);
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(Indexed::Skipped);
    };
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES as u64 {
        return Ok(Indexed::Skipped);
    }
    let mt = mtime_ns(&metadata) as i64;
    let size = metadata.len() as i64;
    let existing: Option<(i64, i64, String)> = conn
        .query_row(
            "SELECT mtime_ns, size, digest FROM files WHERE path=?",
            [&rel],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    if !force
        && existing
            .as_ref()
            .is_some_and(|(m, s, _)| *m == mt && *s == size)
    {
        return Ok(Indexed::Unchanged);
    }
    let Ok(source) = read_source(root, path) else {
        return Ok(Indexed::Skipped);
    };
    let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
    if existing.as_ref().is_some_and(|(_, _, d)| *d == digest) {
        conn.execute(
            "UPDATE files SET mtime_ns=?, size=? WHERE path=?",
            params![mt, size, rel],
        )?;
        return Ok(Indexed::Unchanged);
    }
    let (symbols, refs) = match lang {
        Some(lang) => parse_file(grammars.get(lang)?, &source, lang),
        None => Default::default(),
    };
    // Cascades to symbols, refs and chunks (and chunks_fts via its trigger).
    conn.execute("DELETE FROM files WHERE path=?", [&rel])?;
    conn.execute(
        "INSERT INTO files(path, lang, mtime_ns, size, digest, indexed_at) VALUES(?,?,?,?,?,?)",
        params![
            rel,
            lang.map(langs::name).unwrap_or("text"),
            mt,
            size,
            digest,
            now()
        ],
    )?;
    {
        let mut insert = conn.prepare_cached(
            "INSERT INTO symbols(path, name, kind, line, column, signature) VALUES(?,?,?,?,?,?)",
        )?;
        for (name, kind, line, column, signature) in &symbols {
            insert.execute(params![
                rel,
                name,
                kind,
                *line as i64,
                *column as i64,
                signature
            ])?;
        }
        let mut insert =
            conn.prepare_cached("INSERT INTO refs(path, name, count) VALUES(?,?,?)")?;
        for (name, count) in &refs {
            insert.execute(params![rel, name, count])?;
        }
    }
    let definitions: Vec<(usize, String)> = symbols
        .iter()
        .map(|(name, _, line, _, _)| (*line, name.clone()))
        .collect();
    chunks::store(conn, &rel, &chunks::split(&source, &definitions))?;
    Ok(Indexed::Updated(symbols.len()))
}

/// A stored file that still qualifies for the index, by metadata only.
fn still_indexable(root: &Path, rel: &str) -> bool {
    let full = root.join(rel);
    !crate::redaction::is_secret_path(rel)
        && langs::indexable(&full)
        && fs::metadata(&full).is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE_BYTES as u64)
}

/// Until when each project's last scan is reused.
static LAST_SCAN: Mutex<Option<HashMap<PathBuf, Instant>>> = Mutex::new(None);

/// Index specific relative paths (touched files) and/or a scan (one batch;
/// a scan repeated within a couple of seconds reuses the last one).
pub fn ensure_index(root: &Path, touched: &[String], scan: bool) -> Result<Value> {
    run_index(root, touched, scan, true, MAX_INDEX_FILES)
}

/// One more scan batch now, whatever the last scan (Settings › Reindex).
/// `more` in the answer: files this scan found wait for another batch.
pub fn index_more(root: &Path) -> Result<Value> {
    run_index(root, &[], true, false, MAX_INDEX_FILES)
}

fn run_index(
    root: &Path,
    touched: &[String],
    scan: bool,
    reuse_recent: bool,
    max_files: usize,
) -> Result<Value> {
    ensure!(root.is_dir(), "workspace root required");
    let _guard = INDEX_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("symbol index lock poisoned"))?;
    let conn = open_db(root)?;
    let workspace = Workspace::open(root)?;
    let mut touched_paths = Vec::new();
    let mut touched_rel = HashSet::new();
    for path in touched {
        let rel = workspace.relative(path)?;
        ensure!(
            !crate::redaction::is_secret_path(&rel.to_string_lossy()),
            "Secret paths are not indexed"
        );
        let full = root.join(&rel);
        if full.exists() {
            read_source(root, &full)?;
        }
        touched_rel.insert(relative(root, &full));
        if full.is_file() {
            touched_paths.push(full);
        }
    }
    let wants_scan = scan || touched.is_empty();
    let key = root.canonicalize()?;
    let recent_scan = reuse_recent
        && LAST_SCAN
            .lock()
            .ok()
            .and_then(|scans| scans.as_ref()?.get(&key).copied())
            .is_some_and(|until| Instant::now() < until);
    let focus = meta(&conn, "focus");
    let scan_root = match &focus {
        Some(folder) => root.join(folder),
        None => root.to_path_buf(),
    };
    let (walked, walk_complete) = if wants_scan && !recent_scan {
        walk_sources(&scan_root, max_files)
    } else {
        (Vec::new(), false)
    };
    let scanned = walked.len();
    let transaction = conn.unchecked_transaction()?;
    let mut grammars = Grammars::default();
    let mut indexed_files = 0usize;
    let mut symbol_count = 0usize;
    let mut walked_rel = HashSet::new();
    let mut parsed = 0usize;
    let mut budget_hit = false;
    let forced = touched_paths.iter().map(|p| (p, true));
    let scanned_paths = walked.iter().map(|p| (p, false));
    for (path, force) in forced.chain(scanned_paths) {
        let rel = relative(root, path);
        if !force {
            walked_rel.insert(rel.clone());
            if touched_rel.contains(&rel) {
                continue;
            }
            if parsed >= PARSE_BUDGET {
                // The rest waits for the next batch; the walk still counts it.
                budget_hit = true;
                continue;
            }
        }
        match index_file(&conn, root, path, &mut grammars, force)? {
            Indexed::Updated(n) => {
                indexed_files += 1;
                symbol_count += n;
                parsed += 1;
            }
            Indexed::Unchanged => indexed_files += 1,
            Indexed::Skipped => {}
        }
    }
    let complete = walk_complete && !budget_hit;
    // The project has more files than a scan follows (or than it walks).
    let capped = wants_scan && !recent_scan && !walk_complete;
    if wants_scan && !recent_scan {
        set_meta(&conn, "scan_total", &walked.len().to_string())?;
        set_meta(&conn, "scan_complete", if complete { "1" } else { "0" })?;
        set_meta(&conn, "scan_capped", if capped { "1" } else { "0" })?;
        set_meta(&conn, "scanned_at", &now().to_string())?;
    }
    // Remove stale entries for deleted, renamed, oversized, ignored or secret
    // files. A complete scan is authoritative; otherwise check metadata.
    {
        let stored: Vec<String> = conn
            .prepare("SELECT path FROM files")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for path in stored {
            let keep = if complete {
                walked_rel.contains(&path) || touched_rel.contains(&path)
            } else {
                still_indexable(root, &path)
            };
            if !keep {
                conn.execute("DELETE FROM files WHERE path=?", [path])?;
            }
        }
    }
    transaction.commit()?;
    // Every file the scan found was checked: a scan repeated soon reuses it,
    // for longer when the project is over the limit.
    if wants_scan && !recent_scan && !budget_hit {
        let window = if capped {
            RESCAN_CAPPED_AFTER
        } else {
            RESCAN_AFTER
        };
        if let Ok(mut scans) = LAST_SCAN.lock() {
            scans
                .get_or_insert_with(HashMap::new)
                .insert(key, Instant::now() + window);
        }
    }
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
    Ok(json!({
        "ok": true,
        "indexed_files": indexed_files,
        "symbols_written": symbol_count,
        "symbols_total": total,
        "scanned_files": scanned,
        "parsed_files": parsed,
        "complete": complete,
        "capped": capped,
        "more": budget_hit,
        "bounded": true,
        "focus": focus,
        "storage": if CACHE_DIR.lock().ok().is_some_and(|d| d.is_some()) { "the profile's cache; workspace is unchanged" } else { "private process cache; workspace is unchanged" },
        "note": "Ordinary SQLite tables (tree-sitter symbols, identifier references, FTS5 chunks); at most 2,000 changed files are parsed per call."
    }))
}

/// Index size for status views; does not scan.
pub fn stats(root: &Path) -> Result<Value> {
    let conn = open_db(root)?;
    let count = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    let mut languages = serde_json::Map::new();
    let mut stmt = conn.prepare("SELECT lang, COUNT(*) FROM files GROUP BY lang ORDER BY lang")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (lang, n) = row?;
        languages.insert(lang, json!(n));
    }
    let path = db_path(root)?;
    let size = ["", "-wal"]
        .iter()
        .filter_map(|suffix| {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            fs::metadata(PathBuf::from(name)).ok().map(|m| m.len())
        })
        .sum::<u64>();
    Ok(json!({
        "files": count("SELECT COUNT(*) FROM files")?,
        "symbols": count("SELECT COUNT(*) FROM symbols")?,
        "references": count("SELECT COUNT(*) FROM refs")?,
        "chunks": count("SELECT COUNT(*) FROM chunks")?,
        "languages": languages,
        "max_files": MAX_INDEX_FILES,
        "total": meta(&conn, "scan_total").and_then(|t| t.parse::<u64>().ok()),
        "complete": meta(&conn, "scan_complete").as_deref() == Some("1"),
        "capped": meta(&conn, "scan_capped").as_deref() == Some("1"),
        "focus": meta(&conn, "focus"),
        "size_bytes": size,
        "persistent": CACHE_DIR.lock().ok().is_some_and(|d| d.is_some()),
    }))
}

fn hit_json(path: &str, name: &str, kind: &str, line: i64, column: i64, signature: &str) -> Value {
    json!({
        "path": path,
        "name": name,
        "kind": kind,
        "line": line,
        "column": column,
        "signature": signature
    })
}

pub fn query_definitions(root: &Path, symbol: &str, max_hits: usize) -> Result<Value> {
    let max_hits = max_hits.clamp(1, 80);
    let _ = ensure_index(root, &[], true)?;
    let conn = open_db(root)?;
    let mut stmt = conn.prepare(
        "SELECT path, name, kind, line, column, signature FROM symbols WHERE instr(name, ?1)>0 OR ?1='' ORDER BY name = ?1 DESC,path,line,column LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![symbol, max_hits as i64], |r| {
            Ok(hit_json(
                &r.get::<_, String>(0)?,
                &r.get::<_, String>(1)?,
                &r.get::<_, String>(2)?,
                r.get(3)?,
                r.get(4)?,
                &r.get::<_, String>(5)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    let exact: Vec<_> = rows
        .iter()
        .filter(|h| h["name"].as_str() == Some(symbol))
        .cloned()
        .collect();
    let chosen = if exact.is_empty() { rows } else { exact };
    Ok(json!({
        "ok": !chosen.is_empty(),
        "symbol": symbol,
        "definitions": chosen,
        "source": "sqlite-ast-index",
        "note": if chosen.is_empty() {
            "No definition in the bounded AST index."
        } else {
            "Definitions from on-demand SQLite AST index (tree-sitter)."
        }
    }))
}

pub fn query_references(root: &Path, symbol: &str, max_hits: usize) -> Result<Value> {
    references(root, symbol, max_hits, false)
}

fn is_call(capture: tree_sitter::Node) -> bool {
    let mut node = capture;
    while let Some(parent) = node.parent() {
        if let Some((_, field)) = langs::CALL_KINDS
            .iter()
            .find(|(kind, _)| *kind == parent.kind())
        {
            return parent.child_by_field_name(field).is_some_and(|f| {
                f.start_byte() <= capture.start_byte() && f.end_byte() >= capture.end_byte()
            });
        }
        if !langs::CALLEE_WRAPPERS.contains(&parent.kind()) {
            return false;
        }
        node = parent;
    }
    false
}

fn references(root: &Path, symbol: &str, max_hits: usize, calls_only: bool) -> Result<Value> {
    ensure!(!symbol.is_empty(), "symbol required");
    let max_hits = max_hits.clamp(1, 80);
    let _ = ensure_index(root, &[], true)?;
    // Only files whose identifier counts mention the symbol are parsed again.
    let files: Vec<String> = open_db(root)?
        .prepare("SELECT path FROM refs WHERE name=? ORDER BY path")?
        .query_map([symbol], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut grammars = Grammars::default();
    let mut hits = Vec::new();
    let mut truncated = false;
    'files: for rel in files {
        let path = root.join(&rel);
        let Some(lang) = langs::lang_for(&path) else {
            continue;
        };
        let Ok(source) = read_source(root, &path) else {
            continue;
        };
        let grammar = grammars.get(lang)?;
        let Some(tree) = grammar.parser.parse(&source, None) else {
            continue;
        };
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&grammar.references, tree.root_node(), source.as_bytes());
        while let Some(m) = matches.next() {
            for capture in m.captures {
                let name = capture.node.utf8_text(source.as_bytes()).unwrap_or("");
                if name != symbol || (calls_only && !is_call(capture.node)) {
                    continue;
                }
                if hits.len() >= max_hits {
                    truncated = true;
                    break 'files;
                }
                hits.push(hit_json(
                    &rel,
                    name,
                    "reference",
                    (capture.node.start_position().row + 1) as i64,
                    (capture.node.start_position().column + 1) as i64,
                    "",
                ));
            }
        }
    }
    Ok(json!({
        "ok": true,
        "symbol": symbol,
        "count": hits.len(),
        "truncated": truncated,
        "references": hits,
        "source": "sqlite-ast-index",
        "note": if truncated {
            format!("Reference list capped at {max_hits}; more may exist.")
        } else {
            "Identifier matches from bounded tree-sitter parse over indexed sources.".into()
        }
    }))
}

pub fn get_type_signature(root: &Path, symbol: &str) -> Result<Value> {
    ensure!(!symbol.is_empty(), "symbol required");
    let defs = query_definitions(root, symbol, 8)?;
    let arr = defs["definitions"].as_array().cloned().unwrap_or_default();
    let signatures: Vec<_> = arr
        .iter()
        .filter_map(|d| {
            let sig = d["signature"].as_str().unwrap_or("").trim();
            if sig.is_empty() {
                None
            } else {
                Some(json!({
                    "name": d["name"],
                    "path": d["path"],
                    "line": d["line"],
                    "signature": sig,
                    "kind": d["kind"]
                }))
            }
        })
        .collect();
    Ok(json!({
        "ok": !signatures.is_empty(),
        "symbol": symbol,
        "signatures": signatures,
        "source": "parser",
        "note": if signatures.is_empty() {
            "No parser signature available; rust-analyzer LSP is not required for this tool."
        } else {
            "Signature extracted from the local tree-sitter AST when LSP is absent."
        }
    }))
}

/// Bounded in-repo callers for a function name, for patch preparation context.
pub fn callers_for(root: &Path, symbol: &str, cap: usize) -> Result<Value> {
    let cap = cap.clamp(1, MAX_CALLERS);
    let refs = references(root, symbol, cap, true)?;
    let callers = refs["references"].as_array().cloned().unwrap_or_default();
    let truncated = refs["truncated"].as_bool().unwrap_or(false);
    Ok(json!({
        "symbol": symbol,
        "callers": callers,
        "count": callers.len(),
        "truncated": truncated,
        "cap": cap,
        "note": if truncated {
            format!("Caller list truncated at {cap}; more in-repo references may exist.")
        } else {
            "Syntactic call sites in bounded project sources; receiver types are not resolved.".into()
        }
    }))
}

/// Recently edited files per project, newest first (in-process only).
static RECENT: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());
const MAX_RECENT: usize = 64;

fn remember_edit(root: &Path, rel: &str) {
    let Ok(root) = root.canonicalize() else {
        return;
    };
    if let Ok(mut recent) = RECENT.lock() {
        recent.retain(|(r, p)| !(r == &root && p == rel));
        recent.insert(0, (root, rel.to_owned()));
        recent.truncate(MAX_RECENT);
    }
}

pub fn recent_edits(root: &Path, max: usize) -> Vec<String> {
    let Ok(root) = root.canonicalize() else {
        return Vec::new();
    };
    RECENT
        .lock()
        .map(|recent| {
            recent
                .iter()
                .filter(|(r, _)| r == &root)
                .map(|(_, p)| p.clone())
                .take(max)
                .collect()
        })
        .unwrap_or_default()
}

/// Touch-index a relative path after an edit (no full scan).
pub fn touch(root: &Path, relative_path: &str) -> Result<()> {
    let _guard = INDEX_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("symbol index lock poisoned"))?;
    let conn = open_db(root)?;
    let workspace = Workspace::open(root)?;
    let rel = workspace.relative(relative_path)?;
    let full = root.join(&rel);
    let key = relative(root, &full);
    if !full.exists() {
        // Delete by the normalized key the index stores, not the caller's
        // spelling ("./src/a.rs" must remove "src/a.rs").
        conn.execute("DELETE FROM files WHERE path=?", [key])?;
        return Ok(());
    }
    read_source(root, &full)?;
    let _ = index_file(&conn, root, &full, &mut Grammars::default(), true)?;
    remember_edit(root, &key);
    Ok(())
}

static INDEX_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn indexes_rust_and_returns_signature_and_callers() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "pub fn greet(name: &str) -> String { format!(\"hi {name}\") }\npub fn run() { let _ = greet(\"x\"); }\n",
        )
        .unwrap();
        let status = ensure_index(root.path(), &["src/lib.rs".into()], false).unwrap();
        assert!(status["symbols_total"].as_i64().unwrap() >= 2);
        let sig = get_type_signature(root.path(), "greet").unwrap();
        assert!(sig["ok"].as_bool().unwrap());
        assert!(sig["signatures"][0]["signature"]
            .as_str()
            .unwrap()
            .contains("fn greet"));
        let callers = callers_for(root.path(), "greet", 8).unwrap();
        assert!(callers["count"].as_u64().unwrap() >= 1);
        let defs = query_definitions(root.path(), "greet", 10).unwrap();
        assert!(!defs["definitions"].as_array().unwrap().is_empty());
    }

    #[test]
    fn touch_removes_deleted_files_by_normalized_path() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("gone.rs"), "pub fn vanish() {}\n").unwrap();
        ensure_index(root.path(), &["src/gone.rs".into()], false).unwrap();
        assert!(query_definitions(root.path(), "vanish", 4).unwrap()["ok"] == true);
        fs::remove_file(src.join("gone.rs")).unwrap();
        // A differently spelled path for the same file must still drop the record.
        touch(root.path(), "./src/gone.rs").unwrap();
        let conn = open_db(root.path()).unwrap();
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM symbols WHERE name='vanish'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
        let chunks: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(chunks, 0, "FTS rows follow their file");
    }

    #[test]
    fn does_not_require_startup_world_index() {
        let root = tempfile::tempdir().unwrap();
        let status = ensure_index(root.path(), &[], false).unwrap();
        assert_eq!(status["ok"], true);
        assert!(status["storage"].as_str().unwrap().contains("private"));
        assert!(!root.path().join(".shadow").exists());
    }

    #[test]
    fn indexes_typescript_definitions_references_and_signatures() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("math.ts"),
            "export function add(a: number, b: number): number { return a + b; }\nexport function useAdd() { return add(1, 2); }\n",
        )
        .unwrap();
        let status = ensure_index(root.path(), &["src/math.ts".into()], false).unwrap();
        assert!(status["symbols_total"].as_i64().unwrap() >= 2);
        let defs = query_definitions(root.path(), "add", 10).unwrap();
        assert!(!defs["definitions"].as_array().unwrap().is_empty());
        let refs = query_references(root.path(), "add", 16).unwrap();
        assert!(!refs["references"].as_array().unwrap().is_empty());
        let sig = get_type_signature(root.path(), "add").unwrap();
        assert!(sig["ok"].as_bool().unwrap());
        let callers = callers_for(root.path(), "add", 8).unwrap();
        assert!(callers["count"].as_u64().unwrap() >= 1);
    }

    fn definitions_in(root: &Path, file: &str) -> Vec<(String, String, String)> {
        ensure_index(root, &[file.into()], false).unwrap();
        let conn = open_db(root).unwrap();
        let mut stmt = conn
            .prepare("SELECT name, kind, signature FROM symbols WHERE path=? ORDER BY line")
            .unwrap();
        stmt.query_map([file], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn indexes_python_go_c_cpp_and_java() {
        let root = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| fs::write(root.path().join(name), text).unwrap();
        write(
            "app.py",
            "import os\nLIMIT = 3\nclass Store:\n    def load(self, key):\n        return helper(key)\n\ndef helper(key):\n    return key\n",
        );
        write(
            "main.go",
            "package main\n\ntype Server struct{ port int }\n\nconst Version = \"1\"\n\nfunc (s *Server) Start() error { return run(s.port) }\n\nfunc run(port int) error { return nil }\n",
        );
        write(
            "util.c",
            "#define MAX 4\nstruct point { int x; };\ntypedef int count_t;\nstatic int add(int a, int b) { return a + b; }\nint twice(int a) { return add(a, a); }\n",
        );
        write(
            "shape.hpp",
            "namespace geo {\nclass Shape {\n public:\n  double area() const;\n};\n}\ndouble geo::Shape::area() const { return 0; }\n",
        );
        write(
            "App.java",
            "public class App {\n  public App() {}\n  public int run(int n) { return helper(n); }\n  private int helper(int n) { return n; }\n}\n",
        );
        let names = |file: &str| -> Vec<String> {
            definitions_in(root.path(), file)
                .into_iter()
                .map(|(n, _, _)| n)
                .collect()
        };
        assert_eq!(names("app.py"), ["LIMIT", "Store", "load", "helper"]);
        let python = definitions_in(root.path(), "app.py");
        assert_eq!(python[2].1, "function_definition");
        assert_eq!(python[2].2, "def load(self, key):");
        assert_eq!(names("main.go"), ["Server", "Version", "Start", "run"]);
        assert_eq!(names("util.c"), ["MAX", "point", "count_t", "add", "twice"]);
        let c = definitions_in(root.path(), "util.c");
        assert_eq!(c[3].2, "static int add(int a, int b) { return a + b; }");
        assert_eq!(names("shape.hpp"), ["geo", "Shape", "area", "area"]);
        assert_eq!(names("App.java"), ["App", "App", "run", "helper"]);
        // Callers resolve across grammars' call shapes.
        let callers = callers_for(root.path(), "helper", 8).unwrap();
        let paths: HashSet<_> = callers["callers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["path"].as_str().unwrap().to_owned())
            .collect();
        assert!(
            paths.contains("app.py") && paths.contains("App.java"),
            "{callers}"
        );
        let go = callers_for(root.path(), "run", 8).unwrap();
        assert_eq!(go["count"], 1, "{go}");
        let c_calls = callers_for(root.path(), "add", 8).unwrap();
        assert_eq!(c_calls["callers"][0]["path"], "util.c");
    }

    #[test]
    fn unchanged_files_are_not_reparsed_and_deleted_files_leave_the_index() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.py"), "def one():\n    pass\n").unwrap();
        fs::write(root.path().join("notes.md"), "# Title\nsearchable words\n").unwrap();
        let first = ensure_index(root.path(), &["a.py".into()], true).unwrap();
        assert_eq!(first["symbols_written"], 1);
        let stats = stats(root.path()).unwrap();
        assert_eq!(stats["languages"]["text"], 1, "{stats}");
        assert_eq!(stats["languages"]["python"], 1, "{stats}");
        // A forced touch of an unchanged file keeps the same rows.
        touch(root.path(), "a.py").unwrap();
        assert_eq!(stats_of(root.path())["symbols"], 1);
        fs::remove_file(root.path().join("notes.md")).unwrap();
        fs::write(
            root.path().join("a.py"),
            "def one():\n    pass\ndef two():\n    pass\n",
        )
        .unwrap();
        touch(root.path(), "a.py").unwrap();
        assert_eq!(stats_of(root.path())["symbols"], 2);
        assert_eq!(recent_edits(root.path(), 4), ["a.py"]);
        // Not a scan (the last one was moments ago) but metadata still prunes.
        ensure_index(root.path(), &["a.py".into()], false).unwrap();
        assert_eq!(stats_of(root.path())["files"], 1);
    }

    fn stats_of(root: &Path) -> Value {
        stats(root).unwrap()
    }
    #[test]
    fn a_damaged_or_older_index_is_rebuilt_and_a_good_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite");
        let conn = open_or_rebuild(&path).unwrap();
        conn.execute(
            "INSERT INTO files(path,lang,mtime_ns,size,digest,indexed_at) VALUES('a.rs','rust',1,1,'d',0)",
            [],
        )
        .unwrap();
        drop(conn);
        // Kept across opens (a restart).
        let conn = open_or_rebuild(&path).unwrap();
        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1);
        // Another version: started again.
        conn.execute("UPDATE meta SET value='0' WHERE key='version'", [])
            .unwrap();
        drop(conn);
        let conn = open_or_rebuild(&path).unwrap();
        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 0);
        drop(conn);
        // Damaged: started again.
        remove_db(&path);
        fs::write(&path, b"this is not a database at all, not even close").unwrap();
        let conn = open_or_rebuild(&path).unwrap();
        assert_eq!(meta(&conn, "version").as_deref(), Some(INDEX_VERSION));
    }

    #[test]
    fn large_projects_fill_in_over_batches_and_a_focus_folder_limits_the_scan() {
        let root = tempfile::tempdir().unwrap();
        let count = PARSE_BUDGET + 500;
        for i in 0..count {
            let dir = root.path().join(if i % 2 == 0 { "app" } else { "lib" });
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join(format!("m{i}.rs")),
                format!("pub fn f{i}() {{}}\n"),
            )
            .unwrap();
        }
        let started = Instant::now();
        let first = index_more(root.path()).unwrap();
        assert_eq!(first["parsed_files"], PARSE_BUDGET, "{first}");
        assert_eq!(first["complete"], false);
        assert_eq!(first["more"], true);
        assert_eq!(first["capped"], false);
        let status = stats(root.path()).unwrap();
        assert_eq!(status["total"], count as u64);
        assert_eq!(status["complete"], false);
        let second = index_more(root.path()).unwrap();
        assert_eq!(second["parsed_files"], 500);
        assert_eq!(second["complete"], true);
        assert_eq!(second["more"], false);
        assert_eq!(stats(root.path()).unwrap()["files"], count as i64);
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "{:?}",
            started.elapsed()
        );
        // A focus folder: the other half leaves the index at the next scan.
        set_focus(root.path(), Some("app")).unwrap();
        let focused = index_more(root.path()).unwrap();
        assert_eq!(focused["complete"], true);
        let status = stats(root.path()).unwrap();
        assert_eq!(status["focus"], "app");
        assert_eq!(status["files"], (count / 2 + count % 2) as i64);
        set_focus(root.path(), None).unwrap();
        clear(root.path()).unwrap();
        assert_eq!(stats(root.path()).unwrap()["files"], 0);
    }

    #[test]
    fn a_project_over_the_file_limit_is_walked_once_and_says_so() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..12 {
            fs::write(
                root.path().join(format!("m{i}.rs")),
                format!("pub fn f{i}() {{}}\n"),
            )
            .unwrap();
        }
        let first = run_index(root.path(), &[], true, true, 10).unwrap();
        assert_eq!(first["scanned_files"], 10, "{first}");
        assert_eq!(first["complete"], false);
        assert_eq!(first["capped"], true);
        assert_eq!(
            first["more"], false,
            "every file the scan follows is indexed"
        );
        // Walking it again cannot cover more: a scan soon after reuses it.
        let again = run_index(root.path(), &[], true, true, 10).unwrap();
        assert_eq!(again["scanned_files"], 0, "{again}");
        let status = stats(root.path()).unwrap();
        assert_eq!(status["capped"], true);
        assert_eq!(status["complete"], false);
        clear(root.path()).unwrap();
    }

    #[test]
    fn the_index_of_a_removed_folder_is_cleared_by_its_old_path() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path().join("worktree");
        fs::create_dir(&worktree).unwrap();
        fs::write(worktree.join("a.rs"), "pub fn a() {}\n").unwrap();
        ensure_index(&worktree, &[], true).unwrap();
        let canonical = worktree.canonicalize().unwrap();
        let file = index_path(&canonical);
        assert!(file.exists());
        fs::remove_dir_all(&worktree).unwrap();
        clear(&canonical).unwrap();
        assert!(!file.exists());
    }
}

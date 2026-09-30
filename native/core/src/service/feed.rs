//! The window's live feed and batched diff counts.
//!
//! `GET /api/feed` answers pending approvals and project jobs in one read. The
//! window reads it when the desktop shell forwards an engine broadcast whose
//! type is listed in `events` (approvals requested or resolved, job state
//! changes), with a slow timer only as a backstop; it no longer polls
//! approvals and jobs every second or two.
//!
//! `POST /api/workspace/diffstat` (routed from `workspace.rs`, whose family
//! it belongs to) counts added and removed lines for many changed files with
//! two `git diff --numstat` runs instead of one full diff per file.
use super::*;
use serde_json::Map;
use std::io::Read;

/// Broadcast types after which the feed may have changed. The window also
/// refreshes on untyped wake-ups (a lagged or reattached event stream).
pub const FEED_EVENTS: &[&str] = &[
    "approval.requested",
    "approval.resolved",
    "job.changed",
    "agent.started",
    "agent.completed",
    "agent.paused",
    "agent.resumed",
    "limit.fallback",
    "spend.limit_reached",
    "spend.limit_resolved",
];

/// Paths per diff-count request; a task summary lists at most 20.
const MAX_STAT_PATHS: usize = 200;
/// New files larger than this are counted as unknown instead of read.
const MAX_NEW_FILE_BYTES: u64 = 4 * 1024 * 1024;

type Counts = Option<(u64, u64)>;

impl Service {
    /// The `feed` family.
    pub(super) async fn feed_routes(&self, call: &Arc<Call>) -> Result<Value> {
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/feed") => self.blocking(call, Self::feed).await,
            _ => Err(call.unavailable()),
        }
    }

    fn feed(&self, call: &Call) -> Result<Value> {
        let session = Some(call.q("session_id")).filter(|id| !id.is_empty());
        // Every conversation with a pending approval or a spending card, for
        // sidebar badges.
        let mut waiting: Vec<String> = self
            .engine
            .approvals()
            .list(None)
            .into_iter()
            .map(|approval| approval.session_id)
            .collect();
        waiting.extend(
            self.engine
                .spending_waiting()?
                .iter()
                .filter_map(|card| card["session_id"].as_str().map(str::to_owned)),
        );
        waiting.sort();
        waiting.dedup();
        Ok(json!({
            "waiting": waiting,
            "approvals": self.engine.approvals().list(session),
            "jobs": self.engine.store().job_summaries(call.limit(100, 100))?,
            "events": FEED_EVENTS,
        }))
    }

    /// `{stats: {path: {add, del} | null}}` for the requested paths, as the
    /// window's per-file diff counts them: unstaged plus staged lines, and
    /// every line of a new untracked file. `null` marks binary or unreadable
    /// files; a path without changes counts zero.
    pub(super) async fn diff_stats(&self, body: &Value) -> Result<Value> {
        let requested = body["paths"]
            .as_array()
            .context("List the paths to count")?;
        ensure!(
            requested.len() <= MAX_STAT_PATHS,
            "Count at most {MAX_STAT_PATHS} paths at a time"
        );
        let workspace = Workspace::open(&self.workspace()?)?;
        let mut asked = Vec::new();
        for value in requested {
            let given = value.as_str().context("Paths must be text")?;
            let relative = workspace.relative(given)?.to_string_lossy().into_owned();
            ensure!(relative != ".", "Choose files, not the project folder");
            asked.push((given.to_owned(), relative));
        }
        let mut result = Map::new();
        if asked.is_empty() {
            return Ok(json!({"stats": result}));
        }
        let mut unique: Vec<String> = asked.iter().map(|(_, rel)| rel.clone()).collect();
        unique.sort();
        unique.dedup();
        let counts = count_changes(&workspace.path, &unique).await?;
        for (given, relative) in asked {
            let value = match counts.get(&relative).copied().unwrap_or(Some((0, 0))) {
                Some((add, del)) => json!({"add": add, "del": del}),
                None => Value::Null,
            };
            result.insert(given, value);
        }
        Ok(json!({"stats": result}))
    }
}

/// Unstaged and staged numstat, then new files. Outside a Git repository
/// every path is unknown.
async fn count_changes(root: &Path, paths: &[String]) -> Result<HashMap<String, Counts>> {
    let git = |args: Vec<String>| Service::git_in(root, args, CancellationToken::new());
    let mut counts: HashMap<String, Counts> = HashMap::new();
    for cached in [false, true] {
        let mut args: Vec<String> = [
            "diff",
            "--numstat",
            "-z",
            "--relative",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
        ]
        .map(String::from)
        .into();
        if cached {
            args.push("--cached".into());
        }
        args.push("--".into());
        args.extend(paths.iter().cloned());
        let output = git(args).await?;
        if output["ok"] != true {
            return Ok(paths.iter().map(|p| (p.clone(), None)).collect());
        }
        for (path, stat) in parse_numstat(output["stdout"].as_str().unwrap_or("")) {
            add(&mut counts, path, stat);
        }
    }
    let mut args: Vec<String> = ["ls-files", "--others", "--exclude-standard", "-z", "--"]
        .map(String::from)
        .into();
    args.extend(paths.iter().cloned());
    let others = git(args).await?;
    if others["ok"] == true && others["truncated"] != true {
        let new: Vec<String> = others["stdout"]
            .as_str()
            .unwrap_or("")
            .split('\0')
            .filter(|path| paths.iter().any(|p| p == path))
            .map(str::to_owned)
            .collect();
        // Reading new files is file I/O: keep it off the async workers.
        let root = root.to_owned();
        let lines = tokio::task::spawn_blocking(move || {
            new.into_iter()
                .map(|path| {
                    let lines = new_file_lines(&root.join(&path));
                    (path, lines)
                })
                .collect::<Vec<_>>()
        })
        .await?;
        for (path, stat) in lines {
            add(&mut counts, path, stat);
        }
    }
    Ok(counts)
}

fn add(counts: &mut HashMap<String, Counts>, path: String, stat: Counts) {
    let entry = counts.entry(path).or_insert(Some((0, 0)));
    *entry = match (*entry, stat) {
        (Some((a, d)), Some((more_a, more_d))) => Some((a + more_a, d + more_d)),
        _ => None,
    };
}

/// `git diff --numstat -z` without renames: `added\tdeleted\tpath\0`, with
/// `-` counts for binary files.
fn parse_numstat(text: &str) -> Vec<(String, Counts)> {
    text.split('\0')
        .filter_map(|record| {
            let mut fields = record.splitn(3, '\t');
            let added = fields.next()?;
            let deleted = fields.next()?;
            let path = fields.next().filter(|p| !p.is_empty())?;
            let stat = added.parse().ok().zip(deleted.parse().ok());
            Some((path.to_owned(), stat))
        })
        .collect()
}

/// Every line of a new text file is an added line (as `diff /dev/null`).
/// A symlink is not followed: it may point outside the project.
fn new_file_lines(path: &Path) -> Counts {
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_NEW_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_NEW_FILE_BYTES).read_to_end(&mut bytes).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    let unterminated = u64::from(bytes.last().is_some_and(|b| *b != b'\n'));
    Some((newlines + unterminated, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_records_and_binary_files() {
        let parsed = parse_numstat("3\t1\tsrc/a.rs\0-\t-\timage.png\0\0");
        assert_eq!(
            parsed,
            vec![
                ("src/a.rs".to_owned(), Some((3, 1))),
                ("image.png".to_owned(), None)
            ]
        );
        let mut counts = HashMap::new();
        add(&mut counts, "src/a.rs".into(), Some((3, 1)));
        add(&mut counts, "src/a.rs".into(), Some((1, 0)));
        add(&mut counts, "image.png".into(), None);
        add(&mut counts, "image.png".into(), Some((1, 1)));
        assert_eq!(counts["src/a.rs"], Some((4, 1)));
        assert_eq!(counts["image.png"], None);
    }

    #[test]
    fn new_files_count_every_line() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("new.txt");
        std::fs::write(&text, "one\ntwo\nthree").unwrap();
        assert_eq!(new_file_lines(&text), Some((3, 0)));
        std::fs::write(&text, "").unwrap();
        assert_eq!(new_file_lines(&text), Some((0, 0)));
        let binary = dir.path().join("blob.bin");
        std::fs::write(&binary, [1, 0, 2]).unwrap();
        assert_eq!(new_file_lines(&binary), None);
        assert_eq!(new_file_lines(&dir.path().join("missing")), None);
        #[cfg(unix)]
        {
            let link = dir.path().join("link.txt");
            std::os::unix::fs::symlink(&text, &link).unwrap();
            assert_eq!(new_file_lines(&link), None);
        }
    }
}

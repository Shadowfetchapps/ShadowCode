//! Files the user saved in ShadowCode's editor while a subscription turn was
//! running. A vendor turn is checkpointed as a whole (every file that differs
//! before and after it), so without this a Rewind of the turn would also undo
//! the user's own saves.
//!
//! - A file whose final content is exactly what the user saved, and that the
//!   vendor did not report editing, is the user's: it is not recorded in the
//!   turn's checkpoint, so Rewind never touches it.
//! - A file the user saved that the vendor also edited (it reported the file,
//!   or the content changed after the save) stays in the checkpoint but is
//!   kept by default; the user can choose to rewind it too.
//! - A file that changed during the turn, that the vendor did not report
//!   (while it did report others) and that the user did not save, is flagged:
//!   a command the vendor ran or another program changed it.
use super::Workspace;
use crate::store::{keys, Store};
use anyhow::Result;
use rusqlite::params;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

/// Path (relative to the project) → hash of the user's last save.
type Saves = BTreeMap<String, String>;

/// Note one editor save during `task`'s turn.
pub fn note_save(store: &Store, task: &str, path: &str, hash: &str) -> Result<()> {
    store.update_native_json(&keys::turn_edits(task), |saves: &mut Saves| {
        saves.insert(path.to_owned(), hash.to_owned());
    })
}

/// The editor saves noted during `task`'s turn.
pub fn saves(store: &Store, task: &str) -> Result<Saves> {
    Ok(store
        .native_meta(&keys::turn_edits(task))?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default())
}

/// The project files the vendor reported changing in `task`.
fn vendor_reported(store: &Store, ws: &Workspace, task: &str) -> Result<HashSet<String>> {
    let mut paths = HashSet::new();
    for row in store.query(
        "SELECT payload FROM events WHERE task_id=? AND type='files.changed'",
        [task],
    )? {
        let payload: serde_json::Value = match &row["payload"] {
            serde_json::Value::String(text) => serde_json::from_str(text).unwrap_or_default(),
            other => other.clone(),
        };
        for path in payload["paths"].as_array().into_iter().flatten() {
            if let Some(path) = path.as_str() {
                let relative = ws
                    .relative(path)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| path.to_owned());
                paths.insert(relative);
            }
        }
    }
    Ok(paths)
}

/// After a turn's changes were recorded: take the files that are purely the
/// user's own saves out of the turn's checkpoint. Returns those paths.
pub fn settle(store: &Store, ws: &Workspace, task: &str) -> Result<Vec<String>> {
    let saves = saves(store, task)?;
    if saves.is_empty() {
        return Ok(Vec::new());
    }
    let reported = vendor_reported(store, ws, task)?;
    let mut kept = Vec::new();
    for (path, saved) in &saves {
        let current = ws
            .snapshot(path)
            .ok()
            .and_then(|snapshot| snapshot.hash)
            .unwrap_or_else(|| "missing".into());
        if &current == saved && !reported.contains(path) {
            store.execute(
                "DELETE FROM file_changes WHERE task_id=? AND workspace=? AND path=? AND restored=0",
                params![task, ws.path.to_string_lossy(), path],
            )?;
            kept.push(path.clone());
        }
    }
    Ok(kept)
}

/// "a.txt", "a.txt and b.txt", "a.txt, b.txt and 3 other files".
pub fn describe(paths: &[String]) -> String {
    match paths {
        [] => "no files".into(),
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [one, two, rest @ ..] => format!(
            "{one}, {two} and {} other file{}",
            rest.len(),
            if rest.len() == 1 { "" } else { "s" }
        ),
    }
}

/// Why a rewind leaves a file alone.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Kept {
    pub path: String,
    /// `saved_by_you`: only you changed it during the turn; it is never
    /// rewound. `edited_by_you_and_agent`: you saved it and the agent edited
    /// it too; kept unless you choose to rewind it as well.
    pub reason: &'static str,
}

/// What a rewind of `task` does with each file.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Plan {
    /// Files a default rewind restores.
    pub rewind: Vec<String>,
    /// Files a default rewind leaves as they are.
    pub kept: Vec<Kept>,
    /// Among `rewind`: changed during the turn although the agent did not
    /// report editing them.
    pub unreported: Vec<String>,
}

impl Plan {
    /// Paths a default rewind must leave alone.
    pub fn keep_set(&self) -> HashSet<String> {
        self.kept.iter().map(|kept| kept.path.clone()).collect()
    }
}

pub fn plan(store: &Store, ws: &Workspace, task: &str) -> Result<Plan> {
    let rows = store.query(
        "SELECT path,restored FROM file_changes WHERE task_id=? AND workspace=? ORDER BY id",
        params![task, ws.path.to_string_lossy()],
    )?;
    let recorded: HashSet<String> = rows
        .iter()
        .filter_map(|row| row["path"].as_str().map(str::to_owned))
        .collect();
    let saves = saves(store, task)?;
    let reported = vendor_reported(store, ws, task)?;
    let mut plan = Plan::default();
    for row in rows.iter().filter(|row| row["restored"] != 1) {
        let Some(path) = row["path"].as_str() else {
            continue;
        };
        if saves.contains_key(path) {
            plan.kept.push(Kept {
                path: path.to_owned(),
                reason: "edited_by_you_and_agent",
            });
            continue;
        }
        if !reported.is_empty() && !reported.contains(path) {
            plan.unreported.push(path.to_owned());
        }
        plan.rewind.push(path.to_owned());
    }
    for path in saves.keys().filter(|path| !recorded.contains(*path)) {
        plan.kept.push(Kept {
            path: path.clone(),
            reason: "saved_by_you",
        });
    }
    Ok(plan)
}

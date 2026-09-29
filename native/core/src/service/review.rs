//! `/api/review…`: the per-task review (the files one task changed, their
//! hunks, and undoing a hunk or a file), plus rewinds that can be undone
//! (`POST /api/checkpoints/tasks/<id>/restore` answers an `undo_id`;
//! `POST /api/checkpoints/rewinds/<undo_id>/undo` puts the files back). The
//! work lives in `crate::review`; everything here is synchronous and runs on
//! the blocking pool.
use super::*;

#[derive(Default, Deserialize)]
#[serde(default)]
struct UndoBody {
    path: Text,
    /// A hunk id from the file's review; empty undoes the whole file.
    hunk: Text,
}

impl Service {
    pub(super) async fn review_routes(&self, call: &Arc<Call>) -> Result<Value> {
        self.blocking(call, Self::review_sync).await
    }

    fn review_sync(&self, call: &Call) -> Result<Value> {
        let parts = call.parts();
        if parts.get(2) != Some(&"tasks") || parts.len() < 4 {
            return Err(call.unavailable());
        }
        let task_id = parts[3];
        let (session_id, ws) = self.task_workspace(task_id)?;
        let store = self.engine.store();
        match (call.method.as_str(), parts.get(4).copied()) {
            ("GET", None) => Ok(json!({
                "task_id": task_id,
                "session_id": session_id,
                "workspace": ws.path,
                "busy": self.workspace_busy(&ws.path)?,
                "files": crate::review::files(&store, &ws, task_id)?,
            })),
            ("GET", Some("file")) => crate::review::file(&store, &ws, task_id, call.q("path")),
            ("POST", Some("undo")) => {
                let body: UndoBody = call.body()?;
                let reservation = self.review_workspace(&ws)?;
                let undone = crate::review::undo(
                    &store,
                    &reservation,
                    task_id,
                    body.path.as_str(),
                    body.hunk.non_empty(),
                )?;
                let note = match &undone.hunk {
                    Some(_) => format!(
                        "[Process note from ShadowCode, not a user request: during review the user undid one change this task made in {}. Re-read the file before editing it.]",
                        undone.path
                    ),
                    None => format!(
                        "[Process note from ShadowCode, not a user request: during review the user undid every change this task made in {}. Re-read the file before editing it.]",
                        undone.path
                    ),
                };
                crate::checkpoint::note_in_tape(&store, &session_id, &note)?;
                self.engine.record_event(
                    &session_id,
                    Some(task_id),
                    "review.undone",
                    &json!({"task_id":task_id,"path":undone.path,"hunk":undone.hunk,"whole":undone.hunk.is_none()}),
                )?;
                crate::review::file(&store, &reservation, task_id, &undone.path)
            }
            _ => Err(call.unavailable()),
        }
    }

    /// The conversation and project a task belongs to.
    fn task_workspace(&self, task_id: &str) -> Result<(String, Workspace)> {
        let store = self.engine.store();
        let task = store.task(task_id)?.context("Task not found")?;
        let session_id = task["session_id"]
            .as_str()
            .context("Task has no session")?
            .to_owned();
        let session = store.session(&session_id)?.context("Session not found")?;
        let ws = Workspace::open(Path::new(
            session["workspace"]
                .as_str()
                .context("Session has no workspace")?,
        ))?;
        Ok((session_id, ws))
    }

    /// A task is running or queued in this project: the review is read-only.
    fn workspace_busy(&self, path: &Path) -> Result<bool> {
        let summaries = self.engine.store().job_summaries(100)?;
        Ok(summaries
            .iter()
            .any(|job| active(job) && job["workspace"].as_str().map(Path::new) == Some(path)))
    }

    /// Changing files from the review needs the task's project open, trusted
    /// and writable, and no task running in it.
    fn review_workspace(&self, ws: &Workspace) -> Result<ManualWorkspace> {
        ensure!(
            ws.path == self.workspace()?,
            "Open this task's project before changing its files"
        );
        ensure!(
            !self.workspace_busy(&ws.path)?,
            "Wait for the running task to finish before undoing changes"
        );
        self.mutable_workspace()
    }

    /// POST /api/checkpoints/tasks/<id>/restore: rewind a finished task's
    /// files, keeping what they were so the rewind can be undone.
    pub(super) fn rewind_task(&self, task_id: &str, include_user_edits: bool) -> Result<Value> {
        let (session_id, ws) = self.task_workspace(task_id)?;
        let reservation = self.review_workspace(&ws)?;
        let store = self.engine.store();
        let rewind = crate::review::rewind_with(
            &store,
            &reservation,
            task_id,
            &session_id,
            include_user_edits,
        )?;
        if !rewind.restored.is_empty() {
            crate::checkpoint::note_restore_in_tape(&store, &session_id, &rewind.restored)?;
            self.engine.record_event(
                &session_id,
                Some(task_id),
                "checkpoint.restored",
                &json!({"task_id":task_id,"paths":rewind.restored,"undo_id":rewind.undo_id}),
            )?;
        }
        Ok(json!({"ok":true,"restored":rewind.restored,"undo_id":rewind.undo_id}))
    }

    /// POST /api/checkpoints/rewinds/<undo_id>/undo.
    pub(super) fn undo_rewind(&self, undo_id: &str) -> Result<Value> {
        let store = self.engine.store();
        let record = crate::review::rewind_record(&store, undo_id)?;
        let ws = Workspace::open(Path::new(&record.workspace))?;
        let reservation = self.review_workspace(&ws)?;
        let undone = crate::review::undo_rewind(&store, &reservation, undo_id)?;
        crate::checkpoint::note_in_tape(
            &store,
            &undone.session_id,
            &format!(
                "[Process note from ShadowCode, not a user request: the user undid the rewind, so this task's edits are back on disk for {} path(s): {}. Re-read the files before editing.]",
                undone.restored.len(),
                undone.restored.join(", ")
            ),
        )?;
        self.engine.record_event(
            &undone.session_id,
            Some(&undone.task_id),
            "checkpoint.rewind_undone",
            &json!({"task_id":undone.task_id,"paths":undone.restored,"undo_id":undo_id}),
        )?;
        Ok(json!({"ok":true,"restored":undone.restored,"task_id":undone.task_id}))
    }
}

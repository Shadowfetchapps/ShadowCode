//! `/api/worktree-tasks…`: tasks run in their own managed worktree ("Run in
//! new worktree"). They start through `POST /api/run` with `worktree: true`;
//! these routes list them and apply, keep or discard their result. The
//! engine side lives in `crate::worktree_tasks`.
use super::*;
use crate::worktree_tasks;

impl Service {
    pub(super) async fn worktree_task_routes(&self, call: &Arc<Call>) -> Result<Value> {
        let engine = &self.engine;
        match (call.method.as_str(), &call.parts()[1..]) {
            ("GET", ["worktree-tasks"]) => {
                let workspace = match call.q("workspace") {
                    "" => self.workspace()?,
                    path => Workspace::open(&expand_path(path)?)?.path,
                };
                let records = worktree_tasks::list(engine, &workspace).await?;
                Ok(json!({
                    "workspace": workspace,
                    "tasks": records.iter().map(worktree_tasks::Record::to_json).collect::<Vec<_>>()
                }))
            }
            ("GET", ["worktree-tasks", "setup"]) => {
                let workspace = self.setup_project(call.q("workspace"))?;
                Ok(json!({
                    "workspace": workspace,
                    "setup": worktree_tasks::setup::load(&engine.store(), &workspace),
                    "suggested": worktree_tasks::setup::suggest(&workspace),
                }))
            }
            ("POST", ["worktree-tasks", "setup"]) => {
                let workspace =
                    self.setup_project(call.body["workspace"].as_str().unwrap_or(""))?;
                let wanted: worktree_tasks::setup::Setup =
                    serde_json::from_value(call.body["setup"].clone())
                        .context("setup must be {copy, setup, teardown, port_start, port_end}")?;
                let saved = worktree_tasks::setup::save(&engine.store(), &workspace, wanted)?;
                Ok(json!({"workspace": workspace, "setup": saved}))
            }
            ("GET", ["worktree-tasks", id]) => Ok(worktree_tasks::get(engine, id).await?.to_json()),
            ("POST", ["worktree-tasks", id, action]) => {
                let record = match *action {
                    "apply" => worktree_tasks::apply(engine, id).await?,
                    "keep-branch" => worktree_tasks::keep_branch(engine, id).await?,
                    "discard" => worktree_tasks::discard(engine, id).await?,
                    _ => return Err(call.unavailable()),
                };
                if !record.open() {
                    // The conversation now belongs to the project: when it is
                    // the open one, the window follows it there.
                    let selection = self.snapshot_selection()?;
                    if selection.session.as_deref() == Some(record.session_id.as_str()) {
                        self.select_if(
                            &record.workspace,
                            Some(record.session_id.clone()),
                            Some(selection.generation),
                        )?;
                    }
                    self.engine.store().add_event(
                        "worktree_task.closed",
                        &json!({"id": record.id, "state": record.state, "applied_files": record.applied_files, "branch": record.kept_branch}),
                        Some(&record.session_id),
                        None,
                    )?;
                }
                Ok(record.to_json())
            }
            _ => Err(call.unavailable()),
        }
    }

    /// The project whose worktree setup is read or saved: the one asked for,
    /// else the open one. A worktree's own folder (the open conversation runs
    /// in a worktree task) stands for its project, which new tasks start from.
    fn setup_project(&self, requested: &str) -> Result<PathBuf> {
        let workspace = match requested {
            "" => self.workspace()?,
            path => Workspace::open(&expand_path(path)?)?.path,
        };
        Ok(
            crate::worktrees::source_of_checkout(self.engine.paths(), &workspace)
                .unwrap_or(workspace),
        )
    }
}

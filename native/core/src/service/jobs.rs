//! `/api/jobs…`, `/api/run`, `/api/approvals…` and `/api/checkpoints…`:
//! starting agent and command tasks, controlling running ones, tool
//! approvals and checkpoint rewinds. Starting and cancelling await the
//! engine; everything else is synchronous and runs on the blocking pool.
use super::*;
use crate::store::keys;

/// POST /api/jobs and /api/run.
#[derive(Default, Deserialize)]
#[serde(default)]
struct StartBody {
    workspace: Text,
    task: Text,
    purpose: Text,
    /// A picker id; empty uses the conversation's remembered target.
    model: Text,
    session_id: Text,
    queue: Flag,
    web: Flag,
    handoff_consent: Flag,
    images: Option<Value>,
    permission_limit: Option<Value>,
    /// `low`, `medium`, `high`, or empty for the model's default.
    effort: Text,
    /// `[{path, kind}]` files and folders the prompt @-mentions.
    mentions: Option<Value>,
    /// `[{kind, label, text}]` elements and console messages from the app
    /// preview, appended after the task (`crate::page_context`).
    context: Option<Value>,
    /// Start a new conversation in a fresh managed worktree of the project
    /// (`crate::worktree_tasks`); `session_id` and `queue` are ignored.
    worktree: Flag,
}

/// POST /api/jobs/test.
#[derive(Default, Deserialize)]
#[serde(default)]
struct TestBody {
    workspace: Text,
    command: Text,
    session_id: Text,
    queue: Flag,
    timeout: Loose<u64>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct JobActionBody {
    only_if_queued: Flag,
    instruction: Text,
    path: Text,
    detail: Text,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct DecisionBody {
    session_id: Text,
    decision: Text,
    /// `once` (default) or `task`: allow the same kind of action for the
    /// rest of the task.
    scope: Text,
    /// With `deny`: a reason the model reads.
    note: Text,
}

impl Service {
    pub(super) async fn job_routes(&self, call: &Arc<Call>) -> Result<Value> {
        let parts = call.parts();
        match (call.method.as_str(), call.path.as_str()) {
            ("POST", "/api/jobs/test") => return self.start_test_job(call).await,
            ("POST", "/api/jobs" | "/api/run") => return self.start_job(call).await,
            _ => {}
        }
        if call.method == "GET"
            && call.family() == "jobs"
            && parts.len() == 4
            && parts[3] == "verification"
        {
            let job = self.engine.job(parts[2])?.context("Job not found")?;
            return crate::verification::current(&self.engine, &job).await;
        }
        if call.family() == "jobs"
            && parts.len() >= 3
            && call.method == "POST"
            && parts.get(3) == Some(&"cancel")
        {
            let job = self.engine.job(parts[2])?.context("Job not found")?;
            let body: JobActionBody = call.body()?;
            return Ok(json!(if body.only_if_queued.is_true() {
                self.engine.cancel_queued(&job.id).await?
            } else {
                self.engine.cancel(&job.id).await?
            }));
        }
        self.blocking(call, Self::job_routes_sync).await
    }
    fn job_routes_sync(&self, call: &Call) -> Result<Value> {
        let store = self.engine.store();
        let parts = call.parts();
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/jobs") => {
                return Ok(
                    json!({"jobs":if call.q("view")=="summary" {store.job_summaries(call.limit(100,100))?}else{store.active_and_recent_jobs(1000)?}}),
                )
            }
            ("GET", "/api/jobs/current") => {
                let job = store
                    .current_job(call.q("session_id"), call.q("include_finished") == "true")?;
                return Ok(json!({"job":job}));
            }
            ("GET", "/api/approvals") => {
                return Ok(
                    json!({"approvals":self.engine.approvals().list((!call.q("session_id").is_empty()).then_some(call.q("session_id")))}),
                )
            }
            _ => {}
        }
        match call.family() {
            "jobs" if parts.len() >= 3 => {
                let job = self.engine.job(parts[2])?.context("Job not found")?;
                let body: JobActionBody = call.body()?;
                match (call.method.as_str(), parts.get(3).copied()) {
                    ("GET", None) => Ok(json!(job)),
                    ("POST", Some("pause")) => Ok(json!(self.engine.pause_job(&job.id)?)),
                    ("POST", Some("resume")) => Ok(json!(self.engine.resume_job(&job.id)?)),
                    ("POST", Some("steer")) => Ok(json!(self.engine.steer_job(
                        &job.id,
                        body.instruction.as_str(),
                        body.path.non_empty(),
                    )?)),
                    ("POST", Some("note_edit")) => Ok(json!(self.engine.note_job_edit(
                        &job.id,
                        body.path.as_str(),
                        body.detail.as_str(),
                    )?)),
                    ("POST", Some("rewind")) => self.engine.rewind_job(&job.id),
                    ("GET", Some("events")) => Ok(
                        json!({"events":store.events_after(&job.session_id,call.q("after").parse().unwrap_or(0),job.finished_at.map(|_|job.event_cursor),call.limit(512,2000))?,"job":job}),
                    ),
                    _ => Err(call.unavailable()),
                }
            }
            "approvals" if parts.len() == 3 && call.method == "POST" => {
                let body: DecisionBody = call.body()?;
                let sid = if body.session_id.is_empty() {
                    self.current_session()?
                        .context("Select the task waiting for approval")?
                } else {
                    body.session_id.as_str().to_owned()
                };
                ensure!(
                    matches!(body.decision.as_str(), "approve" | "deny"),
                    "Choose approve or deny"
                );
                ensure!(
                    matches!(body.scope.as_str(), "" | "once" | "task"),
                    "Choose once or task"
                );
                let allow = body.decision.as_str() == "approve";
                Ok(json!(self.engine.approvals().answer(
                    parts[2],
                    &sid,
                    crate::approvals::Answer {
                        allow,
                        for_task: allow && body.scope.as_str() == "task",
                        note: body.note.non_empty().map(str::to_owned),
                        automatic: false,
                    }
                )?))
            }
            "checkpoints" if parts.get(2) == Some(&"tasks") && parts.len() >= 4 => {
                self.checkpoint(call, parts[3])
            }
            "checkpoints"
                if parts.get(2) == Some(&"rewinds")
                    && parts.len() == 5
                    && parts[4] == "undo"
                    && call.method == "POST" =>
            {
                self.undo_rewind(parts[3])
            }
            _ => Err(call.unavailable()),
        }
    }
    /// GET /api/checkpoints/tasks/<id> and POST …/<id>/restore.
    fn checkpoint(&self, call: &Call, task_id: &str) -> Result<Value> {
        let store = self.engine.store();
        let task = store.task(task_id)?.context("Task not found")?;
        let session = store
            .session(task["session_id"].as_str().context("Task has no session")?)?
            .context("Session not found")?;
        let ws = Workspace::open(Path::new(
            session["workspace"]
                .as_str()
                .context("Session has no workspace")?,
        ))?;
        if call.method == "GET" {
            let checkpoint = checkpoint::summary(&store, &ws, task_id)?;
            return Ok(
                json!({"rewindable":checkpoint["changes"].as_u64().unwrap_or(0)>0 && checkpoint["restored"]!=true,"checkpoint":checkpoint}),
            );
        }
        if call.method == "POST" && call.parts().get(4) == Some(&"restore") {
            ensure!(
                ws.path == self.workspace()?,
                "Activate this task's workspace before rewinding"
            );
            // Keeps the files as they are first, so the rewind can be undone.
            return self.rewind_task(task_id);
        }
        Err(call.unavailable())
    }
    /// POST /api/jobs/test: run the project's test command (or the given
    /// one) as a command task.
    async fn start_test_job(&self, call: &Call) -> Result<Value> {
        let body: TestBody = call.body()?;
        let selection = self.snapshot_selection()?;
        let workspace = selection.workspace.clone();
        if let Some(path) = body.workspace.0.as_deref() {
            ensure!(
                Workspace::open(Path::new(path))?.path == workspace,
                "Test task belongs to another workspace"
            );
        }
        let command = if body.command.as_str().trim().is_empty() {
            crate::project::test_command(
                &crate::project::inspect(Arc::new(Workspace::open(&workspace)?)).await?,
            )?
        } else {
            body.command.as_str().to_owned()
        };
        let job = self
            .engine
            .start_command(
                StartRequest {
                    workspace,
                    task: String::new(),
                    session_id: body.session_id.0,
                    model: None,
                    mode: "command".into(),
                    queue: body.queue.is_true(),
                    images: Vec::new(),
                    web: false,
                },
                crate::engine::CommandRequest {
                    command,
                    timeout_sec: body.timeout.0.unwrap_or(300),
                },
                self.job_owner.as_ref(),
            )
            .await?;
        self.select_if(
            &job.workspace,
            Some(job.session_id.clone()),
            Some(selection.generation),
        )?;
        Ok(json!(job))
    }
    /// POST /api/jobs and /api/run: start an agent turn. A provider change
    /// that needs consent answers 409 `needs_consent` without writing.
    async fn start_job(&self, call: &Call) -> Result<Value> {
        let body: StartBody = call.body()?;
        let store = self.engine.store();
        let selection = self
            .selection
            .read()
            .map_err(|_| anyhow::anyhow!("Project lock poisoned"))?
            .clone();
        let workspace = if body.workspace.is_empty() {
            selection.workspace.clone()
        } else {
            Workspace::open(&expand_path(body.workspace.as_str())?)?.path
        };
        let cfg = Config::load(self.engine.paths(), Some(&workspace))?;
        ensure!(
            cfg.is_trusted(&workspace),
            "Trust this project before starting an agent task"
        );
        let purpose = body.purpose.as_str();
        let mode = match purpose {
            "planner" | "plan" | "planning" | "researcher" | "architecture" => "plan",
            "reviewer" | "review" => "review",
            _ => "code",
        };
        let session_id = body.session_id.non_empty().map(str::to_owned).or_else(|| {
            if workspace == selection.workspace {
                selection.session.clone()
            } else {
                None
            }
        });
        // The exact picker id: from the request, else the one this
        // conversation remembers, else the workspace default.
        let target_id = match body.model.as_str() {
            "" => match &session_id {
                Some(sid) => store.session_meta(sid, keys::EXECUTION_TARGET)?,
                None => None,
            }
            .or(store.native_meta(&keys::execution_target(&workspace))?),
            id => Some(id.to_owned()),
        };
        let images = body.images.as_ref().and_then(Value::as_array);
        crate::local_engine::precheck_job(
            &cfg.local_engine,
            target_id.as_deref().unwrap_or(&cfg.model.default),
            images.map_or(0, Vec::len),
        )?;
        crate::openrouter::precheck(
            self.engine.paths(),
            target_id.as_deref().unwrap_or(&cfg.model.default),
            cfg.offline(),
        )?;
        let model = match &target_id {
            Some(id) => Some(self.resolve_model(id, &cfg.model)?),
            None => None,
        };
        let images: Vec<String> = images
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        let task = crate::page_context::append(
            body.task.as_str(),
            &crate::page_context::parse(body.context.as_ref())?,
        );
        let mentions: Vec<crate::mentions::Mention> = match body.mentions.as_ref() {
            Some(Value::Null) | None => Vec::new(),
            Some(value) => serde_json::from_value(value.clone())
                .context("mentions must be a list of {path, kind}")?,
        };
        let turn = crate::engine::TurnOptions {
            effort: crate::effort::parse(body.effort.as_str())?,
            mentions: crate::mentions::validate(&Workspace::open(&workspace)?, mentions)?,
        };
        let mut limit: Option<crate::config::PermissionLevel> = body
            .permission_limit
            .clone()
            .filter(|v| !v.is_null())
            .map(serde_json::from_value)
            .transpose()?;
        // "Run in new worktree": a new conversation in a fresh worktree of
        // this project, which may run beside a task in the main checkout.
        let worktree = if body.worktree.is_true() {
            crate::worktree_tasks::ensure_one_local_model(&self.engine, target_id.as_deref())?;
            let record = crate::worktree_tasks::prepare(
                &self.engine,
                &workspace,
                &task,
                &images,
                model
                    .as_ref()
                    .map_or(cfg.model.default.as_str(), |m| m.default.as_str()),
            )
            .await?;
            // The worktree's own config never widens the project's authority.
            limit = Some(match limit {
                Some(limit) => cfg.permissions.level.restricted_to(limit),
                None => cfg.permissions.level.clone(),
            });
            Some(record)
        } else {
            None
        };
        let (run_in, session_id, queue) = match &worktree {
            Some(record) => (
                record.worktree.clone(),
                Some(record.session_id.clone()),
                false,
            ),
            None => (workspace.clone(), session_id, body.queue.is_true()),
        };
        let started = self
            .engine
            .start_turn_owned(
                StartRequest {
                    workspace: run_in,
                    task: task.clone(),
                    session_id,
                    model,
                    mode: mode.into(),
                    queue,
                    images,
                    web: body.web.is_true(),
                },
                purpose,
                limit,
                self.job_owner.as_ref(),
                body.handoff_consent.is_true(),
                turn,
            )
            .await;
        let (started, worktree) = match (started, worktree) {
            (Ok(job), Some(record)) => {
                match crate::worktree_tasks::started(&self.engine, record, &job).await {
                    Ok(record) => (Ok(job), Some(record)),
                    Err(error) => (Err(error), None),
                }
            }
            (Err(error), Some(record)) => {
                crate::worktree_tasks::abandon(&self.engine, record).await;
                (Err(error), None)
            }
            (started, None) => (started, None),
        };
        let job = match started {
            Ok(job) => job,
            Err(error) => {
                if let Some(consent) =
                    error.downcast_ref::<crate::cli_agent::handoff::ConsentRequired>()
                {
                    // 409: nothing was written; the UI asks and
                    // resends with handoff_consent: true.
                    return Ok(json!({
                        "ok": false,
                        "status": 409,
                        "error": consent.to_string(),
                        "needs_consent": true,
                        "handoff": consent.handoff,
                    }));
                }
                return Err(error);
            }
        };
        if let Some(id) = target_id.filter(|_| !body.model.is_empty()) {
            // Remembered for "keep going on a local model" when a plan runs out.
            // Per project: a worktree task remembers it for its project.
            if id.starts_with("local:gguf:") {
                store.set_native_meta(&keys::last_local_target(&workspace), &id)?;
            }
            store.set_session_meta(&job.session_id, keys::EXECUTION_TARGET, &id)?;
            store.set_native_meta(&keys::execution_target(&workspace), &id)?;
        }
        self.select_if(
            &job.workspace,
            Some(job.session_id.clone()),
            Some(selection.generation),
        )?;
        let mut answer = json!(job);
        if let Some(record) = worktree {
            answer["worktree_task"] = record.to_json();
        }
        Ok(answer)
    }
}

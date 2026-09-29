//! `/api/second-opinions…`: a read-only review of staged changes or of one
//! task's changes by a model the user picks, or another model's view of an
//! answer. The work lives in `crate::second_opinion`; this module gathers
//! what the reviewer is shown (Git for staged changes, the task review for a
//! task) and queues "Ask the agent to fix this" follow-ups.
use super::*;
use crate::second_opinion::{self as opinion, Context as Gathered, Kind, Start};

#[derive(Default, Deserialize)]
#[serde(default)]
struct StartBody {
    kind: Text,
    /// `staged` or `task`.
    source: Text,
    workspace: Text,
    session_id: Text,
    task_id: Text,
    /// Picker id of the reviewer.
    model: Text,
    question: Text,
    consent: Flag,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct PrefsBody {
    workspace: Text,
    model: Text,
    before_commit: Flag,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct FindingBody {
    status: Text,
    consent: Flag,
}

/// The 409 answer the window turns into the consent dialog.
fn needs_consent(error: &anyhow::Error) -> Option<Value> {
    error
        .downcast_ref::<crate::cli_agent::handoff::ConsentRequired>()
        .map(|consent| {
            json!({
                "ok": false,
                "status": 409,
                "error": consent.to_string(),
                "needs_consent": true,
                "handoff": consent.handoff,
            })
        })
}

impl Service {
    pub(super) async fn second_opinion_routes(&self, call: &Arc<Call>) -> Result<Value> {
        let parts = call.parts();
        match (call.method.as_str(), &parts[2..]) {
            ("POST", []) => self.start_second_opinion(call).await,
            ("GET", ["current"]) => {
                let workspace = self.opinion_workspace(call.q("workspace"))?;
                let context = match call.q("source") {
                    "task" => self.task_opinion_context(call.q("task_id")).await?.1,
                    _ => self.staged_opinion_context(&workspace).await?,
                };
                Ok(json!({
                    "hash": context.fingerprint(),
                    "files": context.files,
                    "omitted": context.omitted,
                    "truncated": context.truncated,
                }))
            }
            ("POST", [id, "cancel"]) => Ok(opinion::cancel(&self.engine, id).await?.to_json()),
            ("POST", [id, "findings", finding, "fix"]) => {
                let body: FindingBody = call.body()?;
                self.fix_finding(id, finding, body.consent.is_true()).await
            }
            // The rest reads and writes the database only.
            _ => self.blocking(call, Self::second_opinion_sync).await,
        }
    }

    fn second_opinion_sync(&self, call: &Call) -> Result<Value> {
        let parts = call.parts();
        let engine = &self.engine;
        match (call.method.as_str(), &parts[2..]) {
            ("GET", []) => {
                let workspace = self.opinion_workspace(call.q("workspace"))?;
                let records = opinion::list(
                    engine,
                    &workspace,
                    Some(call.q("session_id")).filter(|s| !s.is_empty()),
                    Some(call.q("task_id")).filter(|s| !s.is_empty()),
                    Some(call.q("source")).filter(|s| !s.is_empty()),
                    call.limit(20, opinion::INDEX_LIMIT),
                )?;
                // The reviewed diff (up to 60 kB each) only when asked for:
                // lists are read every second or two while a review runs.
                let with_diff = call.q("diff") == "1";
                Ok(json!({
                    "workspace": workspace,
                    "second_opinions": records
                        .into_iter()
                        .map(|mut record| {
                            if !with_diff {
                                record.diff.clear();
                            }
                            record.to_json()
                        })
                        .collect::<Vec<_>>(),
                }))
            }
            ("GET", ["options"]) => {
                let workspace = self.opinion_workspace(call.q("workspace"))?;
                self.opinion_options(
                    &workspace,
                    Some(call.q("session_id")).filter(|s| !s.is_empty()),
                    Some(call.q("task_id")).filter(|s| !s.is_empty()),
                )
            }
            ("POST", ["prefs"]) => {
                let body: PrefsBody = call.body()?;
                let workspace = self.opinion_workspace(body.workspace.as_str())?;
                let prefs = opinion::set_prefs(
                    &engine.store(),
                    &workspace,
                    body.model.0.as_deref(),
                    body.before_commit.0,
                )?;
                Ok(json!(prefs))
            }
            ("GET", [id]) => Ok(opinion::get(engine, id)?.to_json()),
            ("POST", [id, "findings", finding]) => {
                let body: FindingBody = call.body()?;
                Ok(opinion::set_finding(engine, id, finding, body.status.as_str())?.to_json())
            }
            _ => Err(call.unavailable()),
        }
    }

    fn opinion_workspace(&self, value: &str) -> Result<PathBuf> {
        Ok(if value.is_empty() {
            self.workspace()?
        } else {
            Workspace::open(&expand_path(value)?)?.path
        })
    }

    /// The staged changes of a project, without secret files.
    async fn staged_opinion_context(&self, workspace: &Path) -> Result<Gathered> {
        let names = Self::git_in(
            workspace,
            ["diff", "--cached", "--name-status", "--no-renames", "-z"]
                .map(str::to_owned)
                .to_vec(),
            CancellationToken::new(),
        )
        .await?;
        ensure!(
            names["ok"] == true,
            "Could not read the staged changes: {}",
            names["stderr"].as_str().unwrap_or("").trim()
        );
        let mut fields = names["stdout"]
            .as_str()
            .unwrap_or("")
            .split('\0')
            .filter(|s| !s.is_empty());
        let mut files: Vec<(String, String)> = Vec::new();
        while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
            files.push((status.to_owned(), path.to_owned()));
        }
        let shown: Vec<String> = files
            .iter()
            .map(|(_, path)| path.clone())
            .filter(|path| !crate::redaction::is_secret_path(path))
            .take(200)
            .collect();
        let mut raw = String::new();
        if !shown.is_empty() {
            let mut args: Vec<String> = [
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--unified=3",
                "--",
            ]
            .map(str::to_owned)
            .to_vec();
            args.extend(shown);
            let diff = Self::git_in(workspace, args, CancellationToken::new()).await?;
            ensure!(
                diff["ok"] == true,
                "Could not read the staged changes: {}",
                diff["stderr"].as_str().unwrap_or("").trim()
            );
            raw = diff["stdout"].as_str().unwrap_or("").to_owned();
        }
        Ok(opinion::staged_context(&files, &raw))
    }

    /// A task's session and changes.
    async fn task_opinion_context(&self, task: &str) -> Result<(String, Gathered, Workspace)> {
        ensure!(!task.is_empty(), "Choose the task to review");
        let (session, ws) = self.task_workspace(task)?;
        let store = self.engine.store();
        let (task, root) = (task.to_owned(), ws.path.clone());
        let context = tokio::task::spawn_blocking(move || {
            opinion::task_context(&store, &Workspace::open(&root)?, &task)
        })
        .await
        .context("The review worker stopped")??;
        Ok((session, context, ws))
    }

    /// What the window needs to suggest a reviewer: the saved preferences,
    /// offline mode, who wrote the change and whether it ran on this
    /// computer (a cloud reviewer then needs consent).
    fn opinion_options(
        &self,
        workspace: &Path,
        session: Option<&str>,
        task: Option<&str>,
    ) -> Result<Value> {
        let store = self.engine.store();
        let cfg = Config::load(self.engine.paths(), Some(workspace))?;
        let ws = Workspace::open(workspace)?;
        let writer = match task {
            Some(task) => opinion::job_of_task(&store, task)?,
            None => match session.map(|s| opinion::recent_writer(&store, &ws, Some(s))) {
                Some(found) => match found? {
                    Some(job) => Some(job),
                    None => opinion::recent_writer(&store, &ws, None)?,
                },
                None => opinion::recent_writer(&store, &ws, None)?,
            },
        };
        let writer = writer
            .as_ref()
            .and_then(crate::cli_agent::handoff::TurnRoute::of_job);
        let conversation = match task
            .and_then(|task| store.task(task).ok().flatten())
            .and_then(|task| task["session_id"].as_str().map(str::to_owned))
            .or(session.map(str::to_owned))
        {
            Some(session) => store
                .session_jobs(&session, 200)?
                .iter()
                .rev()
                .find_map(crate::cli_agent::handoff::TurnRoute::of_job),
            None => None,
        };
        let local_only = writer.as_ref().is_some_and(|w| w.local)
            || conversation.as_ref().is_some_and(|c| c.local);
        Ok(json!({
            "workspace": workspace,
            "prefs": opinion::prefs(&store, workspace)?,
            "offline": cfg.offline(),
            "writer": writer.map(|w| json!({"model": w.model_id, "label": w.label, "local": w.local})),
            "local_only": local_only,
        }))
    }

    /// POST /api/second-opinions.
    async fn start_second_opinion(&self, call: &Call) -> Result<Value> {
        let body: StartBody = call.body()?;
        let kind = Kind::parse(body.kind.as_str())?;
        let store = self.engine.store();
        let source = match body.source.as_str() {
            "" if !body.task_id.is_empty() => "task",
            "" => "staged",
            other => other,
        }
        .to_owned();
        let (workspace, session, mut context, writer_job, task_id) = match source.as_str() {
            "task" => {
                let task = body.task_id.as_str().to_owned();
                let (session, context, ws) = self.task_opinion_context(&task).await?;
                if !body.workspace.is_empty() {
                    ensure!(
                        self.opinion_workspace(body.workspace.as_str())? == ws.path,
                        "That task belongs to another project"
                    );
                }
                let job = opinion::job_of_task(&store, &task)?;
                (ws.path, Some(session), context, job, Some(task))
            }
            "staged" => {
                let workspace = self.opinion_workspace(body.workspace.as_str())?;
                let ws = Workspace::open(&workspace)?;
                let context = self.staged_opinion_context(&workspace).await?;
                // The conversation the user is in, when it is in this project.
                let session = match body.session_id.non_empty() {
                    Some(id) => {
                        let row = store.session(id)?.context("Session not found")?;
                        ensure!(
                            row["workspace"].as_str() == workspace.to_str(),
                            "Session belongs to a different project"
                        );
                        Some(id.to_owned())
                    }
                    None => None,
                };
                let job = match opinion::recent_writer(&store, &ws, session.as_deref())? {
                    Some(job) => Some(job),
                    None => opinion::recent_writer(&store, &ws, None)?,
                };
                let session = session.or_else(|| {
                    job.as_ref()
                        .and_then(|j| j["session_id"].as_str().map(str::to_owned))
                });
                (workspace, session, context, job, None)
            }
            _ => bail!("Choose what to review: staged or task"),
        };
        if kind == Kind::Ask {
            if let Some(job) = &writer_job {
                opinion::answer_context(&mut context, job);
            }
        }
        let started = opinion::start(
            &self.engine,
            Start {
                kind,
                source,
                workspace,
                session_id: session,
                task_id,
                model: body.model.as_str().to_owned(),
                question: body.question.as_str().to_owned(),
                consent: body.consent.is_true(),
                context,
                writer_job,
                owner: self.job_owner.as_ref(),
            },
        )
        .await;
        match started {
            Ok(record) => Ok(record.to_json()),
            Err(error) => needs_consent(&error).ok_or(error),
        }
    }

    /// POST /api/second-opinions/<id>/findings/<finding>/fix: queue a
    /// follow-up in the conversation the review belongs to (a new
    /// conversation when there is none), on that conversation's model.
    async fn fix_finding(&self, id: &str, finding: &str, consent: bool) -> Result<Value> {
        let record = opinion::get(&self.engine, id)?;
        let item = opinion::finding(&record, finding)?.clone();
        ensure!(
            item.status != "fixing",
            "A fix for this finding is already queued"
        );
        let store = self.engine.store();
        let workspace = record.workspace.clone();
        let cfg = Config::load(self.engine.paths(), Some(&workspace))?;
        ensure!(
            cfg.is_trusted(&workspace),
            "Trust this project before starting an agent task"
        );
        let session = match &record.session_id {
            Some(sid) => store
                .session(sid)?
                .filter(|row| row["workspace"].as_str() == workspace.to_str())
                .map(|_| sid.clone()),
            None => None,
        };
        let target = match &session {
            Some(sid) => store.session_meta(sid, crate::store::keys::EXECUTION_TARGET)?,
            None => None,
        }
        .or(store.native_meta(&crate::store::keys::execution_target(&workspace))?);
        let target_id = target.as_deref().unwrap_or(&cfg.model.default);
        crate::local_engine::precheck_job(&cfg.local_engine, target_id, 0)?;
        crate::openrouter::precheck(self.engine.paths(), target_id, cfg.offline())?;
        let model = match &target {
            Some(id) => Some(self.resolve_model(id, &cfg.model)?),
            None => None,
        };
        let started = self
            .engine
            .start_turn_owned(
                StartRequest {
                    workspace,
                    task: opinion::fix_prompt(&record, &item),
                    session_id: session,
                    model,
                    mode: "code".into(),
                    queue: true,
                    images: Vec::new(),
                    web: false,
                },
                "coder",
                None,
                self.job_owner.as_ref(),
                consent,
                crate::engine::TurnOptions::default(),
            )
            .await;
        let job = match started {
            Ok(job) => job,
            Err(error) => return needs_consent(&error).ok_or(error),
        };
        let record = opinion::mark_fixing(&self.engine, id, finding, &job.id, &job.session_id)?;
        Ok(json!({"second_opinion": record.to_json(), "job": job}))
    }
}

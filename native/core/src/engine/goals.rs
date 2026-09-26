use super::*;
use crate::tools::truncate;

pub(super) struct GoalRun {
    pub session_id: String,
    /// The picker target the goal runs on (conversation, else workspace
    /// default), resolved once so every milestone uses the same route.
    pub model: Option<crate::config::ModelConfig>,
    pub cancel: CancellationToken,
    pub finished: AtomicBool,
    done: Notify,
}
impl GoalRun {
    pub(super) async fn wait(&self) {
        loop {
            let notified = self.done.notified();
            if self.finished.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
    }
}
impl Engine {
    pub fn start_goal(&self, goal_id: &str, session_id: Option<&str>) -> Result<Value> {
        let mut runs = self
            .0
            .goals
            .lock()
            .map_err(|_| anyhow!("Goal registry lock poisoned"))?;
        ensure!(
            !self.0.closing.load(Ordering::Acquire),
            "Application is shutting down"
        );
        runs.retain(|_, run| !run.finished.load(Ordering::Acquire));
        ensure!(!runs.contains_key(goal_id), "This goal is already running");
        ensure!(runs.len() < 16, "At most 16 goals may be running");
        let goal = self.0.store.goal(goal_id)?;
        ensure!(
            goal["milestones"]
                .as_array()
                .is_some_and(|ms| ms.iter().any(|m| m["status"] != "done")),
            "All milestones are already complete"
        );
        let workspace = Workspace::open(Path::new(
            goal["workspace"]
                .as_str()
                .context("Goal workspace missing")?,
        ))?;
        let config = Config::load(self.paths(), Some(&workspace.path))?;
        // start_with_context enforces this for every milestone; refusing here
        // keeps an untrusted goal from being marked running at all.
        ensure!(
            config.is_trusted(&workspace.path),
            "Trust this project before running a goal"
        );
        // Goals run on the model picked in the composer: the conversation's
        // execution target, else the project default. Never the config model.
        let target = session_id
            .map(str::to_owned)
            .or_else(|| goal["session_id"].as_str().map(str::to_owned))
            .map(|sid| self.0.store.session_meta(&sid, keys::EXECUTION_TARGET))
            .transpose()?
            .flatten()
            .or(self
                .0
                .store
                .native_meta(&keys::execution_target(&workspace.path))?);
        let model = match &target {
            Some(id) => Some(crate::model_registry::resolve(
                &self.0.store,
                id,
                &config.model,
            )?),
            None => None,
        };
        ensure!(
            model.as_ref().unwrap_or(&config.model).provider != "mock",
            "Choose a model in the composer before running a goal"
        );
        let sid = match session_id {
            Some(sid) => {
                let session = self.0.store.session(sid)?.context("Session not found")?;
                ensure!(
                    session["workspace"].as_str() == workspace.path.to_str(),
                    "Session belongs to a different workspace"
                );
                sid.to_owned()
            }
            None => match goal["session_id"].as_str() {
                Some(sid) if self.0.store.session(sid)?.is_some() => sid.to_owned(),
                _ => self.0.store.create_session(
                    &workspace.path,
                    &config.model.default,
                    goal["title"].as_str().unwrap_or("Goal"),
                )?["id"]
                    .as_str()
                    .context("Session ID missing")?
                    .to_owned(),
            },
        };
        let goal = self.0.store.begin_goal(goal_id, &sid)?;
        let run = Arc::new(GoalRun {
            session_id: sid,
            model,
            cancel: CancellationToken::new(),
            finished: AtomicBool::new(false),
            done: Notify::new(),
        });
        runs.insert(goal_id.into(), run.clone());
        let engine = self.clone();
        let goal_id = goal_id.to_owned();
        let worker_goal = goal.clone();
        tokio::spawn(async move {
            let outcome = std::panic::AssertUnwindSafe(engine.drive_goal(&worker_goal, &run))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(anyhow!(
                        "Goal worker interrupted unexpectedly; review saved work before resuming"
                    ))
                });
            let (status, detail) = match outcome {
                Ok(value) => value,
                Err(error) => (
                    if run.cancel.is_cancelled() {
                        "paused"
                    } else {
                        "blocked"
                    },
                    format!("{error:#}"),
                ),
            };
            let saved = engine
                .0
                .store
                .finish_goal(&goal_id, status, truncate(&detail, 16000));
            let payload = match saved {
                Ok(()) => json!({"goal_id":goal_id,"status":status,"detail":detail}),
                Err(error) => {
                    json!({"goal_id":goal_id,"status":"error","detail":format!("Could not save goal state: {error:#}")})
                }
            };
            if let Ok(event) =
                engine
                    .0
                    .store
                    .add_event("goal.updated", &payload, Some(&run.session_id), None)
            {
                let _ = engine.0.sender.send(event);
            }
            run.finished.store(true, Ordering::Release);
            run.done.notify_waiters();
        });
        Ok(goal)
    }
    async fn drive_goal(&self, goal: &Value, run: &GoalRun) -> Result<(&'static str, String)> {
        let goal_id = goal["id"].as_str().context("Goal ID missing")?;
        let milestones = goal["milestones"]
            .as_array()
            .context("Milestones missing")?;
        for milestone in milestones {
            if run.cancel.is_cancelled() {
                return Ok((
                    "paused",
                    "Goal paused; completed milestones were retained.".into(),
                ));
            }
            if milestone["status"] == "done" {
                continue;
            }
            let mid = milestone["id"].as_str().context("Milestone ID missing")?;
            let verify = milestone["require_verification"].as_bool().unwrap_or(false);
            let current = self.0.store.goal(goal_id)?;
            let checklist = current["milestones"]
                .as_array()
                .context("Milestones missing")?
                .iter()
                .map(|m| {
                    format!(
                        "- {} ({})",
                        m["title"].as_str().unwrap_or(""),
                        m["status"].as_str().unwrap_or("pending")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let checklist = truncate(&checklist, 8000);
            let task = format!(
                "{}\n\nGoal: {}",
                milestone["title"].as_str().unwrap_or(""),
                goal["instruction"].as_str().unwrap_or("")
            );
            let context=format!("Goal milestone checklist:\n{checklist}\n\nWork on the current milestone toward the original goal. Preserve earlier completed work. Report concrete evidence and unresolved limitations.{}",
                if verify {" Run the relevant acceptance checks with the terminal. This milestone requires a successful recorded command; a prose assertion alone cannot complete it. If checks fail or cannot run, report that honestly."}else{""});
            let job = self
                .start_with_context(
                    StartRequest {
                        workspace: PathBuf::from(
                            goal["workspace"].as_str().context("Workspace missing")?,
                        ),
                        task,
                        session_id: Some(run.session_id.clone()),
                        model: run.model.clone(),
                        mode: milestone["mode"].as_str().unwrap_or("code").into(),
                        queue: true,
                        images: Vec::new(),
                        web: false,
                    },
                    LaunchContext {
                        system_context: Some(context),
                        purpose: if verify { "tester" } else { "" },
                        ..Default::default()
                    },
                )
                .await?;
            if let Err(error) = self
                .0
                .store
                .goal_milestone_started(goal_id, mid, &json!(job))
            {
                self.cancel(&job.id).await?;
                return Err(error);
            }
            if let Ok(event) = self.0.store.add_event(
                "goal.milestone.started",
                &json!({"goal_id":goal_id,"milestone_id":mid,"job_id":job.id}),
                Some(&run.session_id),
                Some(&job.task_id),
            ) {
                let _ = self.0.sender.send(event);
            }
            let outcome = tokio::select! {
                biased;
                _=run.cancel.cancelled()=>self.cancel(&job.id).await?,
                outcome=self.wait(&job.id)=>outcome?,
            };
            if run.cancel.is_cancelled() || outcome.status == "cancelled" {
                self.0.store.set_milestone(
                    goal_id,
                    mid,
                    "pending",
                    truncate(&outcome.summary, 16000),
                )?;
                return Ok((
                    "paused",
                    "Goal paused; review partial changes before resuming.".into(),
                ));
            }
            let verification = outcome
                .result
                .as_ref()
                .and_then(|r| r.pointer("/verification/status"))
                .and_then(Value::as_str)
                .unwrap_or("not_run");
            let execution_failed = outcome
                .result
                .as_ref()
                .is_some_and(|r| r.pointer("/verification/execution_failed") == Some(&json!(true)));
            let passed = outcome.status == "completed"
                && !execution_failed
                && !matches!(verification, "failed" | "cancelled" | "stale")
                && (!verify || verification == "passed");
            let detail = if outcome.status == "completed" && !passed {
                format!(
                    "Milestone requires attention: verification is {verification}.\n{}",
                    outcome.summary
                )
            } else {
                outcome.summary
            };
            self.0.store.set_milestone(
                goal_id,
                mid,
                if passed { "done" } else { "failed" },
                truncate(&detail, 16000),
            )?;
            if !passed {
                return Ok(("blocked", detail));
            }
        }
        Ok((
            "completed",
            "All milestones completed. Review the recorded task and verification evidence.".into(),
        ))
    }
    pub async fn wait_goal(&self, goal_id: &str) -> Result<Value> {
        let run = self
            .0
            .goals
            .lock()
            .map_err(|_| anyhow!("Goal registry lock poisoned"))?
            .get(goal_id)
            .cloned();
        if let Some(run) = run {
            run.wait().await;
        }
        self.0.store.goal(goal_id)
    }
    pub async fn stop_goal(&self, goal_id: &str, abandon: bool) -> Result<Value> {
        let run = {
            let runs = self
                .0
                .goals
                .lock()
                .map_err(|_| anyhow!("Goal registry lock poisoned"))?;
            let run = runs.get(goal_id).cloned();
            if let Some(run) = &run {
                run.cancel.cancel();
            }
            if run.is_none() {
                self.0.store.finish_goal(
                    goal_id,
                    if abandon { "abandoned" } else { "paused" },
                    "",
                )?;
            }
            run
        };
        if let Some(run) = run {
            run.wait().await;
            let runs = self
                .0
                .goals
                .lock()
                .map_err(|_| anyhow!("Goal registry lock poisoned"))?;
            ensure!(
                runs.get(goal_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &run)),
                "Goal was resumed before this stop request completed"
            );
            if abandon {
                self.0
                    .store
                    .finish_goal(goal_id, "abandoned", "Goal abandoned by the user.")?;
            }
        }
        self.0.store.goal(goal_id)
    }
    pub fn update_goal_milestone(
        &self,
        goal_id: &str,
        mid: &str,
        status: &str,
        detail: &str,
    ) -> Result<Value> {
        let runs = self
            .0
            .goals
            .lock()
            .map_err(|_| anyhow!("Goal registry lock poisoned"))?;
        ensure!(
            runs.get(goal_id)
                .is_none_or(|r| r.finished.load(Ordering::Acquire)),
            "Pause the goal before editing milestones"
        );
        self.0.store.set_milestone(
            goal_id,
            mid,
            status,
            if detail.is_empty() && status == "done" {
                "Marked complete by the user."
            } else {
                detail
            },
        )
    }
    pub fn delete_goal(&self, goal_id: &str) -> Result<()> {
        let mut runs = self
            .0
            .goals
            .lock()
            .map_err(|_| anyhow!("Goal registry lock poisoned"))?;
        ensure!(
            runs.get(goal_id)
                .is_none_or(|r| r.finished.load(Ordering::Acquire)),
            "Pause the goal before deleting it"
        );
        self.0.store.delete_goal(goal_id)?;
        runs.remove(goal_id);
        Ok(())
    }
}

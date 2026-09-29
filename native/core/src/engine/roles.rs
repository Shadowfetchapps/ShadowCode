//! Plan → Implement → Review tasks (`crate::roles`). The task runs its roles
//! one after another, each as a child with its own conversation (a vendor
//! CLI job or ShadowCode's own loop, `crate::subagents`):
//!
//! 1. the plan role inspects the project read-only and writes a plan;
//! 2. the implement role changes an isolated worktree and returns a diff;
//! 3. the review role reviews that diff read-only (it is not applied yet);
//! 4. the task applies the diff to the project with `apply_agent_changes`,
//!    an ordinary checkpointed patch that asks for approval in Ask mode.
//!
//! The plan and implement roles receive a bounded summary of the
//! conversation so far (the model-switch handoff, `cli_agent::handoff::build`)
//! and the request; later roles also receive the plan, and the review role
//! the diff. A Plan task runs the plan role only.
//! Approvals of every role are asked in this conversation with the role's
//! name; stopping the task stops the running role; every role's usage counts
//! toward this task.
use super::*;
use crate::{
    cli_agent::handoff::{self, TurnRoute},
    roles::{self, Pipeline, Role, Target},
    subagents::{ChildOutcome, SubagentsConfig},
};

/// A conversation summary no role has seen: every earlier turn is unseen.
fn summary_route() -> TurnRoute {
    TurnRoute {
        provider: "shadowcode:roles:summary".into(),
        model_id: String::new(),
        label: "the roles".into(),
        local: false,
    }
}

/// Progress of the task's roles, shown as its plan.
struct Progress {
    steps: Vec<Value>,
}

impl Progress {
    fn new(pipeline: &Pipeline, apply: bool) -> Self {
        let mut steps: Vec<Value> = pipeline
            .stages
            .iter()
            .map(|stage| {
                json!({
                    "id": stage.role.id(),
                    "title": format!("{} · {}", stage.role.label(), stage.name()),
                    "status": "pending",
                    "detail": "",
                })
            })
            .collect();
        if apply {
            steps.push(json!({
                "id": "apply",
                "title": "Apply the changes",
                "status": "pending",
                "detail": "",
            }));
        }
        Self { steps }
    }
    fn set(&mut self, id: &str, status: &str, detail: &str) {
        if let Some(step) = self.steps.iter_mut().find(|s| s["id"] == id) {
            step["status"] = json!(status);
            step["detail"] = json!(crate::tools::truncate(detail, 400));
        }
    }
    fn plan(&self) -> Value {
        json!({"goal": "Plan → Implement → Review", "steps": self.steps})
    }
    fn emit(&self, events: &TaskEvents) -> Result<()> {
        events.emit("plan.updated", json!({"plan": self.plan()}))?;
        Ok(())
    }
}

/// What one role did, for the summary and `roles.finished`.
struct StageResult {
    target: Target,
    outcome: Option<ChildOutcome>,
    /// The role could not start (its run has no card).
    error: Option<String>,
    skipped: Option<&'static str>,
}

impl StageResult {
    fn status(&self) -> &str {
        match (&self.outcome, self.skipped) {
            (_, Some(_)) => "skipped",
            (Some(outcome), _) => outcome.record.status.as_str(),
            (None, _) => "failed",
        }
    }
    fn completed(&self) -> bool {
        self.status() == "completed"
    }
    fn to_json(&self) -> Value {
        let record = self.outcome.as_ref().map(|o| &o.record);
        json!({
            "role": self.target.role.id(),
            "label": self.target.role.label(),
            "name": self.target.name(),
            "model_id": self.target.model.default,
            "runner": self.target.runner(),
            "vendor": self.target.vendor().map(crate::cli_agent::Vendor::id),
            "route": if self.target.local() { "local" } else { "cloud" },
            "cost": self.target.cost(),
            "status": self.status(),
            "skipped": self.skipped,
            "error": self.error.clone().or_else(|| record.and_then(|r| r.error.clone())).map(|e| crate::tools::truncate(&e, 600).to_owned()),
            "run_id": record.map(|r| r.id.clone()),
            "session_id": record.map(|r| r.session_id.clone()),
            "usage": record.map(|r| r.usage.clone()).unwrap_or(Value::Null),
            "files": record.map_or(0, |r| r.files.len()),
            "additions": record.map_or(0, |r| r.files.iter().map(|f| f.additions).sum::<u64>()),
            "deletions": record.map_or(0, |r| r.files.iter().map(|f| f.deletions).sum::<u64>()),
            "verdict": record.and_then(|r| r.verdict.clone()),
            "duration_s": record.map(|r| ((r.finished_at.unwrap_or(r.created_at) - r.created_at) * 10.0).round() / 10.0),
        })
    }
    /// One line of the task's answer.
    fn line(&self) -> String {
        let who = format!("**{}** · {}", self.target.role.label(), self.target.name());
        if let Some(reason) = self.skipped {
            return format!("- {who} — skipped: {reason}");
        }
        let Some(outcome) = &self.outcome else {
            return format!(
                "- {who} — could not start: {}",
                crate::tools::truncate(self.error.as_deref().unwrap_or("unknown error"), 400)
            );
        };
        let record = &outcome.record;
        let status = match record.status.as_str() {
            "completed" => "done".to_owned(),
            "cancelled" => "stopped".to_owned(),
            "limit_reached" => "plan limit reached".to_owned(),
            other => other.replace('_', " "),
        };
        let mut line = format!("- {who} — {status}");
        if self.target.role == Role::Implement && record.status == "completed" {
            if record.files.is_empty() {
                line.push_str(", no file changes");
            } else {
                let (add, del) = record
                    .files
                    .iter()
                    .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
                line.push_str(&format!(
                    ", {} file{} changed (+{add} −{del})",
                    record.files.len(),
                    if record.files.len() == 1 { "" } else { "s" }
                ));
                // Binary files cannot go through the patch; say so.
                match record.binary_files.len() {
                    0 => {}
                    1 => line.push_str(&format!(
                        "; the binary file {} is not applied",
                        record.binary_files[0]
                    )),
                    n => line.push_str(&format!("; {n} binary files are not applied")),
                }
            }
        }
        if let Some(verdict) = &record.verdict {
            line.push_str(match verdict.as_str() {
                "ready" => ", verdict: ready",
                _ => ", verdict: needs changes",
            });
        }
        if record.status != "completed" {
            if let Some(error) = &record.error {
                line.push_str(&format!(": {}", crate::tools::truncate(error, 400)));
            }
        }
        line
    }
}

/// The parts of a start request the role checks read.
pub(super) struct RoleTurn<'a> {
    pub mode: &'a str,
    pub task: &'a str,
    pub images: usize,
    pub session: Option<&'a str>,
}

impl Engine {
    /// Resolve and check a Plan → Implement → Review task before any job or
    /// task row exists. Returns the pipeline and the cloud providers it sends
    /// work to, recorded as allowed for the conversation once the job exists.
    /// A cloud role in a conversation that ran on this computer (or that has
    /// earlier turns another provider has not seen) needs consent first:
    /// `handoff::ConsentRequired`, which the window answers with its consent
    /// dialog.
    pub(super) fn prepare_roles(
        &self,
        request: RoleTurn<'_>,
        workspace: &Path,
        config: &Config,
        consent: bool,
    ) -> Result<(Pipeline, Vec<String>)> {
        ensure!(
            matches!(request.mode, "code" | "plan"),
            "Plan → Implement → Review runs Code and Plan tasks. Turn it off under More › Roles to ask a question."
        );
        ensure!(
            request.images == 0,
            "Images can't be handed to roles yet. Remove the images, or turn off Plan → Implement → Review under More › Roles for this message."
        );
        let store = &self.0.store;
        let project = roles::project_of(store, workspace, request.session);
        let setup = roles::load(store, &project)?;
        let pipeline = Pipeline::build(store, &setup, &config.model, request.mode)?;
        if request.mode == "code" {
            ensure!(
                config.permissions.level != PermissionLevel::ReadOnly,
                "This project is read-only, so the implement role can't change files. Use Plan mode, or change the project's permissions."
            );
        }
        let jobs = match request.session {
            Some(sid) => store.session_jobs(sid, 200)?,
            None => Vec::new(),
        };
        let prior: Vec<TurnRoute> = jobs
            .iter()
            .filter(|job| job["mode"] != "command")
            .filter_map(TurnRoute::of_job)
            .collect();
        let conversation_local =
            handoff::is_local(&config.model) || prior.last().is_some_and(|r| r.local);
        let consented = request
            .session
            .map(|sid| roles::consented(store, sid))
            .unwrap_or_default();
        // Offline and turned-off vendors are refused outright; consent is
        // decided below for the whole task at once.
        let guard = roles::Guard {
            config,
            conversation_local: false,
            consented: &consented,
        };
        let mut ask = false;
        for stage in &pipeline.stages {
            if let Err(roles::Refusal::Blocked(reason)) = guard.check(stage) {
                bail!(reason);
            }
            let provider = &stage.model.provider;
            if !stage.local()
                && !consented.contains(provider)
                && (conversation_local || prior.iter().any(|r| &r.provider != provider))
            {
                ask = true;
            }
        }
        if ask && !consent {
            let tasks: Vec<String> = jobs
                .iter()
                .filter_map(|j| j["task_id"].as_str().map(str::to_owned))
                .collect();
            let excerpt = handoff::build(&jobs, &summary_route(), &store.changed_files(&tasks)?)
                .map_or(0, |h| h.excerpt_chars());
            let from = prior
                .last()
                .map(|r| r.label.clone())
                .unwrap_or_else(|| roles::model_label(&config.model));
            let mut refusal = roles::consent_request(&pipeline.stages, &from, excerpt);
            if !conversation_local {
                refusal.reason = format!(
                    "earlier turns ran on {from}; {}",
                    refusal.reason.split_once("; ").map_or("", |(_, rest)| rest)
                );
                refusal.handoff["reason"] = json!(refusal.reason);
            }
            return Err(refusal.into());
        }
        let providers = pipeline.cloud_providers();
        Ok((pipeline, providers))
    }

    /// An `@agent` request that its role or definition sends to a cloud
    /// model, in a conversation that runs on this computer: ask first, like
    /// any other turn that moves local work to the cloud. Returns the
    /// provider to record once the user agreed.
    pub(super) fn mention_consent(
        &self,
        request: RoleTurn<'_>,
        workspace: &Workspace,
        config: &Config,
        consent: bool,
    ) -> Result<Option<String>> {
        if !request.task.contains('@')
            || crate::cli_agent::Vendor::from_provider(&config.model.provider).is_some()
            || !handoff::is_local(&config.model)
            || config.offline()
        {
            return Ok(None);
        }
        let settings = SubagentsConfig::from_config(config);
        if !settings.enabled || settings.max_depth == 0 {
            return Ok(None);
        }
        let catalog = crate::agents::discover_for(&self.0.paths, workspace);
        let Some((definition, _)) = crate::agents::mention(request.task, &catalog) else {
            return Ok(None);
        };
        let store = &self.0.store;
        let setting = match &definition.model {
            Some(id) => Some((None, id.clone())),
            None => Role::for_agent(definition).and_then(|role| {
                let project = roles::project_of(store, &workspace.path, request.session);
                let setup = roles::load(store, &project).ok()?;
                let id = setup.get(role).trim();
                (!matches!(id, "" | roles::SKIP)).then(|| (Some(role), id.to_owned()))
            }),
        };
        let Some((role, id)) = setting else {
            return Ok(None);
        };
        // An unknown model is reported by the run itself.
        let Ok(model) = crate::model_registry::resolve(store, &id, &config.model) else {
            return Ok(None);
        };
        if handoff::is_local(&model) {
            return Ok(None);
        }
        let consented = request
            .session
            .map(|sid| roles::consented(store, sid))
            .unwrap_or_default();
        if consented.contains(&model.provider) {
            return Ok(None);
        }
        if !consent {
            let name = roles::model_label(&model);
            let what = match role {
                Some(role) => format!("the @{} subagent (the {} role)", definition.name, role.id()),
                None => format!("the @{} subagent", definition.name),
            };
            let reason = format!(
                "this conversation runs on this computer ({}); {what} runs on {name} in the cloud and will receive its task and read the project's files",
                roles::model_label(&config.model)
            );
            return Err(handoff::ConsentRequired {
                handoff: json!({
                    "from": roles::model_label(&config.model),
                    "to": name,
                    "excerpt_chars": 0,
                    "images": 0,
                    "reason": reason,
                    "roles": [{"role": role.map(Role::id), "label": role.map(Role::label), "name": name, "provider": model.provider, "agent": definition.name}],
                }),
                reason,
            }
            .into());
        }
        Ok(Some(model.provider))
    }

    /// The conversation so far, as the bounded handoff block every role
    /// receives (none for a new conversation).
    fn roles_prior(&self, job: &Job) -> Result<Option<String>> {
        let store = &self.0.store;
        let jobs: Vec<Value> = store
            .session_jobs(&job.session_id, 200)?
            .into_iter()
            .filter(|j| j["id"] != json!(job.id))
            .collect();
        let tasks: Vec<String> = jobs
            .iter()
            .filter_map(|j| j["task_id"].as_str().map(str::to_owned))
            .collect();
        Ok(handoff::build(&jobs, &summary_route(), &store.changed_files(&tasks)?).map(|h| h.text))
    }

    pub(super) async fn run_roles(
        &self,
        running: &Running,
        job: Job,
        events: TaskEvents,
        tools: &ToolExecutor,
        pipeline: &Pipeline,
    ) -> Result<(String, Value)> {
        let host = tools
            .extensions()
            .host
            .clone()
            .context("Plan → Implement → Review could not start its roles")?;
        events.emit(
            "agent.started",
            json!({
                "job_id": job.id,
                "task": job.task,
                "mode": job.mode,
                "model": job.model,
                "native": true,
                "images": job.images,
                "roles": pipeline.to_json(),
            }),
        )?;
        if let Some(decision) = &job.routing {
            events.emit("routing.selected", json!(decision))?;
        }
        events.emit(
            "roles.started",
            json!({"label": pipeline.label(), "stages": pipeline.to_json()}),
        )?;
        let implements = pipeline.stage(Role::Implement).is_some();
        let mut progress = Progress::new(pipeline, implements);
        progress.emit(&events)?;
        let prior = self.roles_prior(&job)?;
        let request = job.task.clone();
        let mut results: Vec<StageResult> = Vec::new();
        let mut plan: Option<(String, String)> = None;
        for stage in &pipeline.stages {
            ensure!(!running.cancel.is_cancelled(), "Task cancelled");
            let implemented = results
                .iter()
                .find(|r| r.target.role == Role::Implement)
                .and_then(|r| r.outcome.as_ref());
            let (prompt, write) = match stage.role {
                Role::Plan => (roles::plan_prompt(&request, prior.as_deref()), false),
                Role::Implement => (
                    roles::implement_prompt(
                        &request,
                        prior.as_deref(),
                        plan.as_ref()
                            .map(|(who, text)| (who.as_str(), text.as_str())),
                    ),
                    true,
                ),
                Role::Review => {
                    let Some(done) = implemented.filter(|o| o.record.patch) else {
                        progress.set(stage.role.id(), "done", "Nothing to review");
                        results.push(StageResult {
                            target: stage.clone(),
                            outcome: None,
                            error: None,
                            skipped: Some("the implement role made no changes"),
                        });
                        continue;
                    };
                    let files: Vec<String> = done
                        .record
                        .files
                        .iter()
                        .map(|f| {
                            format!(
                                "{} {} (+{} -{})",
                                f.status, f.path, f.additions, f.deletions
                            )
                        })
                        .collect();
                    (
                        roles::review_prompt(
                            &request,
                            plan.as_ref()
                                .map(|(who, text)| (who.as_str(), text.as_str())),
                            &done.record.model,
                            &files,
                            &done.diff,
                        ),
                        false,
                    )
                }
                Role::Explore => continue,
            };
            progress.set(stage.role.id(), "running", "");
            progress.emit(&events)?;
            let result = match host
                .run_role(stage, prompt, write, stage.role.label())
                .await
            {
                Ok(outcome) => StageResult {
                    target: stage.clone(),
                    outcome: Some(outcome),
                    error: None,
                    skipped: None,
                },
                Err(error) => StageResult {
                    target: stage.clone(),
                    outcome: None,
                    error: Some(format!("{error:#}")),
                    skipped: None,
                },
            };
            let ok = result.completed();
            progress.set(
                stage.role.id(),
                if ok { "done" } else { "failed" },
                if ok { "" } else { result.status() },
            );
            if ok && stage.role == Role::Plan {
                if let Some(outcome) = &result.outcome {
                    plan = Some((stage.name(), outcome.record.summary.clone()));
                }
            }
            results.push(result);
            // A plan or an implementation that did not finish ends the task;
            // a review that did not finish leaves the decision to the user.
            if !ok && stage.role != Role::Review {
                break;
            }
        }
        ensure!(!running.cancel.is_cancelled(), "Task cancelled");
        // Apply the implement role's diff with the usual edit approval.
        let mut applied = None;
        let implemented = results
            .iter()
            .find(|r| r.target.role == Role::Implement)
            .and_then(|r| r.outcome.as_ref().map(|o| (r.completed(), &o.record)));
        let mut apply_note = String::new();
        match implemented {
            Some((true, record)) if record.patch => {
                progress.set("apply", "running", "");
                progress.emit(&events)?;
                let call = crate::models::ToolCall {
                    id: crate::id(),
                    name: "apply_agent_changes".into(),
                    arguments: json!({"run_id": record.id}),
                };
                let result = tools.execute(call).await?;
                ensure!(!running.cancel.is_cancelled(), "Task cancelled");
                if result.success {
                    progress.set("apply", "done", "");
                    apply_note = "The changes were applied to the project.".into();
                    applied = Some(true);
                } else {
                    progress.set("apply", "failed", &result.error);
                    apply_note = format!(
                        "The changes were not applied: {}. The Implement card lists them; run the task again to redo them.",
                        crate::tools::truncate(&result.error, 400).trim_end_matches('.'),
                    );
                    applied = Some(false);
                }
            }
            Some((true, record)) => {
                progress.set("apply", "done", "No changes");
                apply_note = if record.binary_files.is_empty() {
                    "The implement role made no changes.".into()
                } else {
                    format!(
                        "The implement role changed only binary files, which are not applied: {}.",
                        record.binary_files.join(", ")
                    )
                };
            }
            Some((false, record)) if record.patch => {
                progress.set("apply", "failed", "Not applied");
                apply_note = "The implement role did not finish, so its partial changes were not applied. They are listed on its card.".into();
                applied = Some(false);
            }
            _ if implements => {
                progress.set("apply", "failed", "Nothing to apply");
                apply_note = "Nothing was changed.".into();
            }
            _ => {}
        }
        progress.emit(&events)?;
        let finished = results.iter().all(|r| r.completed() || r.skipped.is_some());
        let mut summary = String::new();
        if job.mode == "plan" {
            // A Plan task answers with the plan itself.
            match results.first() {
                Some(result) if result.completed() => {
                    summary.push_str(&format!(
                        "**Plan** · {}\n\n{}",
                        result.target.name(),
                        result
                            .outcome
                            .as_ref()
                            .map_or("", |o| o.record.summary.as_str())
                    ));
                }
                Some(result) => summary.push_str(&result.line()),
                None => summary.push_str("No role ran."),
            }
        } else {
            summary.push_str("**Plan → Implement → Review**\n\n");
            for result in &results {
                summary.push_str(&result.line());
                summary.push('\n');
            }
            if !apply_note.is_empty() {
                summary.push('\n');
                summary.push_str(&apply_note);
                summary.push('\n');
            }
            if let Some(review) = results
                .iter()
                .find(|r| r.target.role == Role::Review && r.completed())
                .and_then(|r| r.outcome.as_ref())
            {
                summary.push_str(&format!(
                    "\n### Review ({})\n\n{}\n",
                    review.record.model,
                    crate::tools::truncate(&review.record.summary, 6_000)
                ));
            } else if let Some((who, text)) = &plan {
                summary.push_str(&format!(
                    "\n### Plan ({who})\n\n{}\n",
                    crate::tools::truncate(text, 3_000)
                ));
            }
        }
        let files: Vec<String> = results
            .iter()
            .filter(|r| r.target.role == Role::Implement)
            .filter_map(|r| r.outcome.as_ref())
            .flat_map(|o| o.record.files.iter().map(|f| f.path.clone()))
            .collect();
        events.emit(
            "roles.finished",
            json!({
                "label": pipeline.label(),
                "stages": results.iter().map(StageResult::to_json).collect::<Vec<_>>(),
                "applied": applied,
                "apply_note": apply_note,
                "files": files,
                "completed": finished && applied != Some(false),
            }),
        )?;
        // Keep the conversation's message tape complete, so a later turn on
        // ShadowCode's own loop sees this task as plain prior conversation.
        let mut tape = self
            .0
            .store
            .latest_session_messages(&job.session_id, &job.id)?;
        tape.retain(|m| m["role"] != "system" || m["_shadow_compaction"] == true);
        tape.push(json!({"role":"user","content":job.task}));
        tape.push(json!({
            "role":"assistant",
            "content":format!("[{}]\n{}", pipeline.label(), crate::tools::truncate(&summary, 8000)),
        }));
        self.save_tape(&job.id, &tape).await?;
        // A stage that failed fails the task; its summary says which.
        if let Some(failed) = results
            .iter()
            .find(|r| !r.completed() && r.skipped.is_none() && r.target.role != Role::Review)
        {
            bail!(
                "{}\n\n{summary}",
                match failed.target.role {
                    Role::Plan => format!(
                        "The plan role ({}) did not finish, so nothing was changed.",
                        failed.target.name()
                    ),
                    _ => format!(
                        "The implement role ({}) did not finish.",
                        failed.target.name()
                    ),
                }
            );
        }
        Ok((summary, progress.plan()))
    }
}

//! The automation scheduler and runs. The desktop and `shadowcode serve`
//! call [`Engine::start_automations`]; one-shot CLI commands never do, so a
//! `shadowcode run` never fires somebody's schedule.
//!
//! A run is a normal job in a new conversation tagged with the automation
//! (`session_meta` keys `automation_id` / `automation_run`), by default in a
//! fresh managed worktree. At most one run per automation exists at a time:
//! the registry below is checked and filled under one lock, so a schedule
//! tick and "Run now" can never both start one. A run that asks for approval
//! stops (or waits, if the automation says so), and every run has a time
//! limit. The result, duration and usage land in `automation_runs`.
use super::*;
use crate::tools::truncate;
use crate::{
    automations::{self as rules, Automation, Checkout, Due, OnApproval, Permission},
    store::AutomationRun,
};
use std::{collections::HashSet, sync::Weak};

/// How often the scheduler looks for due automations.
const TICK: Duration = Duration::from_secs(20);

#[derive(Default)]
pub(super) struct Registry {
    /// automation id → its active run.
    runs: Mutex<HashMap<String, Arc<Active>>>,
    scheduler: AtomicBool,
    /// Ticks never overlap (the scheduler, and tests that tick by hand).
    tick: tokio::sync::Mutex<()>,
}

pub(super) struct Active {
    run_id: String,
    cancel: CancellationToken,
    finished: AtomicBool,
    done: Notify,
}
impl Active {
    async fn wait(&self) {
        loop {
            let notified = self.done.notified();
            if self.finished.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
    }
}

impl Registry {
    /// Cancel every active run (shutdown) and return them to wait on.
    pub(super) fn cancel_all(&self) -> Vec<Arc<Active>> {
        let runs: Vec<_> = self
            .runs
            .lock()
            .map(|runs| runs.values().cloned().collect())
            .unwrap_or_default();
        for run in &runs {
            run.cancel.cancel();
        }
        runs
    }
    pub(super) async fn wait_all(runs: Vec<Arc<Active>>) {
        for run in runs {
            run.wait().await;
        }
    }
}

fn job_outcome(status: &str) -> &'static str {
    match status {
        "completed" => "completed",
        "cancelled" => "cancelled",
        "interrupted" => "interrupted",
        _ => "failed",
    }
}

/// A worktree with no edits: only ignored files or ShadowCode's own
/// `.shadow/` folder.
fn untouched(status: &str) -> bool {
    status.lines().all(|line| {
        line.starts_with("!! ")
            || line
                .get(3..)
                .is_some_and(|path| path.starts_with(".shadow/"))
    })
}

fn set_trust(engine: &Engine, path: &Path, trusted: bool) -> Result<()> {
    Config::update(engine.paths(), |cfg| {
        if trusted {
            cfg.grant_trust(path);
        } else {
            cfg.trusted_workspaces
                .retain(|stored| Path::new(stored.trim()) != path);
        }
        Ok(())
    })?;
    Ok(())
}

impl Engine {
    /// Start the scheduler loop (desktop and `serve`). Calling it again does
    /// nothing. The first tick runs at once: that is when times missed while
    /// ShadowCode was closed are caught up or recorded as missed.
    pub fn start_automations(&self) {
        if self.0.automations.scheduler.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak: Weak<Inner> = Arc::downgrade(&self.0);
        tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { break };
                let engine = Engine(inner);
                if engine.0.closing.load(Ordering::Acquire) {
                    break;
                }
                if let Err(error) = engine.automation_tick(crate::now()).await {
                    tracing::warn!("Automations: {error:#}");
                }
                drop(engine);
                tokio::time::sleep(TICK).await;
            }
        });
    }
    /// Whether this engine runs schedules (false for one-shot CLI engines).
    pub fn automations_scheduled(&self) -> bool {
        self.0.automations.scheduler.load(Ordering::Acquire)
    }

    /// Look at every enabled automation whose time has come, as of `now`:
    /// run it, run it late (catch-up), or record it as missed; then move its
    /// next time past `now`. Returns the history rows it wrote.
    pub async fn automation_tick(&self, now: f64) -> Result<Vec<AutomationRun>> {
        let _tick = self.0.automations.tick.lock().await;
        // One-shot continuations after a plan limit resets.
        if let Err(error) = self.resume_tick(now).await {
            tracing::warn!("Scheduled resumes: {error:#}");
        }
        let store = self.0.store.clone();
        let due = store.run(move |s| s.due_automations(now)).await?;
        let mut rows = Vec::new();
        for automation in due {
            let scheduled = automation.next_run_at.unwrap_or(now);
            let next = rules::next_run(&automation.schedule, automation.timezone, now)
                .ok()
                .flatten();
            // Advance first, so a failure below never fires again each tick.
            let id = automation.id.clone();
            store
                .run(move |s| s.set_automation_next_run(&id, next))
                .await?;
            let row = match rules::decide(scheduled, now, automation.options.catch_up_minutes) {
                Due::NotYet => continue,
                Due::Missed => {
                    let missed = rules::missed_count(
                        &automation.schedule,
                        automation.timezone,
                        scheduled,
                        now,
                    );
                    let row = AutomationRun {
                        id: crate::id(),
                        automation_id: automation.id.clone(),
                        status: "missed".into(),
                        trigger: "schedule".into(),
                        scheduled_for: Some(scheduled),
                        started_at: now,
                        finished_at: Some(now),
                        missed,
                        detail: format!(
                            "ShadowCode was not running at the scheduled time{}. It is older than the {} minute catch-up window, so it did not run.",
                            if missed > 1 { format!(" ({missed} times)") } else { String::new() },
                            automation.options.catch_up_minutes
                        ),
                        ..Default::default()
                    };
                    self.save_run(row.clone()).await?;
                    row
                }
                Due::Run { catch_up } => {
                    let trigger = if catch_up { "catch_up" } else { "schedule" };
                    match self
                        .launch_automation(&automation, trigger, Some(scheduled))
                        .await
                    {
                        Ok(Some(run)) => run,
                        Ok(None) => {
                            let row = AutomationRun {
                                id: crate::id(),
                                automation_id: automation.id.clone(),
                                status: "skipped".into(),
                                trigger: trigger.into(),
                                scheduled_for: Some(scheduled),
                                started_at: now,
                                finished_at: Some(now),
                                detail:
                                    "The previous run was still going, so this time was skipped."
                                        .into(),
                                ..Default::default()
                            };
                            self.save_run(row.clone()).await?;
                            row
                        }
                        Err(error) => {
                            let row = AutomationRun {
                                id: crate::id(),
                                automation_id: automation.id.clone(),
                                status: "failed".into(),
                                trigger: trigger.into(),
                                scheduled_for: Some(scheduled),
                                started_at: now,
                                finished_at: Some(now),
                                detail: format!("{error:#}"),
                                ..Default::default()
                            };
                            self.save_run(row.clone()).await?;
                            row
                        }
                    }
                }
            };
            rows.push(row);
        }
        Ok(rows)
    }

    /// "Run now": the same run a schedule starts, unless one is going.
    pub async fn run_automation_now(&self, id: &str) -> Result<AutomationRun> {
        let store = self.0.store.clone();
        let id = id.to_owned();
        let automation = store.run(move |s| s.automation(&id)).await?;
        self.launch_automation(&automation, "manual", None)
            .await?
            .context("This automation is already running")
    }

    /// The active run's id, if one is going.
    pub fn automation_active_run(&self, id: &str) -> Option<String> {
        let runs = self.0.automations.runs.lock().ok()?;
        runs.get(id)
            .filter(|run| !run.finished.load(Ordering::Acquire))
            .map(|run| run.run_id.clone())
    }

    /// Stop the active run (its job is cancelled) and wait for it to be
    /// recorded.
    pub async fn stop_automation(&self, id: &str) -> Result<AutomationRun> {
        let active = self
            .0
            .automations
            .runs
            .lock()
            .map_err(|_| anyhow!("Automation registry lock poisoned"))?
            .get(id)
            .cloned()
            .filter(|run| !run.finished.load(Ordering::Acquire))
            .context("This automation is not running")?;
        active.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(60), active.wait())
            .await
            .context("The run is still stopping; check again shortly")?;
        let run_id = active.run_id.clone();
        let store = self.0.store.clone();
        store.run(move |s| s.automation_run(&run_id)).await
    }

    /// Wait for the active run (if any) to finish and return its history
    /// row; with none active, the newest row.
    pub async fn wait_automation(&self, id: &str) -> Result<Option<AutomationRun>> {
        let active = self
            .0
            .automations
            .runs
            .lock()
            .map_err(|_| anyhow!("Automation registry lock poisoned"))?
            .get(id)
            .cloned();
        let store = self.0.store.clone();
        if let Some(active) = active {
            active.wait().await;
            let run_id = active.run_id.clone();
            return Ok(Some(store.run(move |s| s.automation_run(&run_id)).await?));
        }
        let id = id.to_owned();
        Ok(store
            .run(move |s| s.automation_runs(&id, 1))
            .await?
            .into_iter()
            .next())
    }

    pub fn delete_automation(&self, id: &str) -> Result<()> {
        let runs = self
            .0
            .automations
            .runs
            .lock()
            .map_err(|_| anyhow!("Automation registry lock poisoned"))?;
        ensure!(
            runs.get(id)
                .is_none_or(|run| run.finished.load(Ordering::Acquire)),
            "Stop the running automation before deleting it"
        );
        self.0.store.delete_automation(id)
    }

    async fn save_run(&self, run: AutomationRun) -> Result<()> {
        let store = self.0.store.clone();
        store.run(move |s| s.save_automation_run(&run)).await
    }

    async fn automation_event(&self, kind: &'static str, payload: Value, session: Option<String>) {
        let store = self.0.store.clone();
        if let Ok(event) = store
            .run(move |s| s.add_event(kind, &payload, session.as_deref(), None))
            .await
        {
            let _ = self.0.sender.send(event);
        }
    }

    /// Reserve the automation, write its `running` row and start the worker.
    /// `None` when a run of this automation is already going.
    async fn launch_automation(
        &self,
        automation: &Automation,
        trigger: &str,
        scheduled_for: Option<f64>,
    ) -> Result<Option<AutomationRun>> {
        let run = AutomationRun {
            id: crate::id(),
            automation_id: automation.id.clone(),
            status: "running".into(),
            trigger: trigger.into(),
            scheduled_for,
            started_at: crate::now(),
            ..Default::default()
        };
        let active = Arc::new(Active {
            run_id: run.id.clone(),
            cancel: CancellationToken::new(),
            finished: AtomicBool::new(false),
            done: Notify::new(),
        });
        {
            let mut runs = self
                .0
                .automations
                .runs
                .lock()
                .map_err(|_| anyhow!("Automation registry lock poisoned"))?;
            ensure!(
                !self.0.closing.load(Ordering::Acquire),
                "Application is shutting down"
            );
            runs.retain(|_, run| !run.finished.load(Ordering::Acquire));
            if runs.contains_key(&automation.id) {
                return Ok(None);
            }
            runs.insert(automation.id.clone(), active.clone());
        }
        let release = |engine: &Engine| {
            if let Ok(mut runs) = engine.0.automations.runs.lock() {
                runs.remove(&automation.id);
            }
            active.finished.store(true, Ordering::Release);
            active.done.notify_waiters();
        };
        if let Err(error) = self.save_run(run.clone()).await {
            release(self);
            return Err(error);
        }
        let engine = self.clone();
        let automation = automation.clone();
        let worker_active = active.clone();
        let mut record = run.clone();
        tokio::spawn(async move {
            let active = worker_active;
            let outcome = std::panic::AssertUnwindSafe(engine.drive_automation(
                &automation,
                &mut record,
                &active,
            ))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(anyhow!("The automation stopped unexpectedly")));
            if let Err(error) = outcome {
                record.status = if active.cancel.is_cancelled() {
                    "cancelled".into()
                } else {
                    "failed".into()
                };
                let text = format!("{error:#}");
                record.detail = if record.detail.is_empty() {
                    text
                } else {
                    format!("{text}\n{}", record.detail)
                };
            }
            record.finished_at = Some(crate::now());
            if let Err(error) = engine.save_run(record.clone()).await {
                tracing::warn!("Could not save automation run: {error:#}");
            }
            engine
                .automation_event(
                    "automation.finished",
                    json!({
                        "automation_id": automation.id,
                        "run_id": record.id,
                        "name": automation.name,
                        "status": record.status,
                        "notify": automation.options.notify,
                        "summary": truncate(&record.summary, 400),
                        "detail": truncate(&record.detail, 400),
                    }),
                    record.session_id.clone(),
                )
                .await;
            active.finished.store(true, Ordering::Release);
            active.done.notify_waiters();
        });
        Ok(Some(run))
    }

    async fn drive_automation(
        &self,
        automation: &Automation,
        run: &mut AutomationRun,
        active: &Active,
    ) -> Result<()> {
        let store = self.0.store.clone();
        let workspace = Workspace::open(&automation.workspace)
            .context("The project folder of this automation is missing")?
            .path;
        let config = Config::load(self.paths(), Some(&workspace))?;
        ensure!(
            config.is_trusted(&workspace),
            "Trust this project before its automations can run"
        );
        let target = match automation.model.trim() {
            "" => store.native_meta(&keys::execution_target(&workspace))?,
            id => Some(id.to_owned()),
        };
        let model = match &target {
            Some(id) => Some(crate::model_registry::resolve(&store, id, &config.model)?),
            None => None,
        };
        let chosen = model.as_ref().unwrap_or(&config.model);
        ensure!(
            chosen.provider != "mock",
            "Choose a model for this automation, or pick one in the composer for this project"
        );
        let model_label = chosen.default.clone();

        // Where it runs.
        let worktree = match automation.options.checkout {
            Checkout::Main => None,
            Checkout::Worktree => {
                let record = crate::worktrees::create(
                    self.paths(),
                    &workspace,
                    "HEAD",
                    active.cancel.clone(),
                )
                .await
                .context("Could not create a fresh worktree for this run. The project must be the top folder of a Git repository with at least one commit; or set the automation to run in the main checkout")?;
                let path = record.path.canonicalize()?;
                set_trust(self, &path, true)?;
                run.worktree = Some(json!({
                    "id": record.id,
                    "path": path,
                    "branch": record.branch,
                    "base_commit": record.base_commit,
                }));
                Some((record, path))
            }
        };
        let folder = worktree
            .as_ref()
            .map(|(_, path)| path.clone())
            .unwrap_or_else(|| workspace.clone());

        let started = self
            .start_automation_job(
                automation,
                run,
                &folder,
                target.as_deref(),
                model,
                &model_label,
                worktree.is_some(),
            )
            .await;
        let job = match started {
            Ok(job) => job,
            Err(error) => {
                if let Some((record, path)) = &worktree {
                    self.discard_worktree(&workspace, &record.id, path).await;
                }
                return Err(error);
            }
        };
        self.automation_event(
            "automation.started",
            json!({"automation_id": automation.id, "run_id": run.id, "name": automation.name, "job_id": job.id}),
            Some(job.session_id.clone()),
        )
        .await;

        // Watch it: finished, stopped, out of time, or asking for approval.
        let limit = Duration::from_secs(u64::from(automation.options.max_runtime_minutes) * 60);
        let deadline = tokio::time::Instant::now() + limit;
        let mut announced = HashSet::new();
        let (status, detail) = loop {
            tokio::select! {
                finished = self.wait(&job.id) => {
                    let finished = finished?;
                    break (job_outcome(&finished.status).to_owned(), String::new());
                }
                _ = active.cancel.cancelled() => {
                    self.cancel(&job.id).await?;
                    break ("cancelled".into(), if self.0.closing.load(Ordering::Acquire) {
                        "ShadowCode was closing, so the run was stopped.".into()
                    } else {
                        "Stopped by you.".into()
                    });
                }
                _ = tokio::time::sleep_until(deadline) => {
                    self.cancel(&job.id).await?;
                    break ("timed_out".into(), format!(
                        "Stopped at the {} minute time limit.",
                        automation.options.max_runtime_minutes
                    ));
                }
                _ = tokio::time::sleep(Duration::from_millis(400)) => {
                    if let Some((meter, card)) = self.running(&job.id)?.and_then(|r| {
                        let meter = r.spend.clone()?;
                        let card = meter.pending()?;
                        Some((meter, card))
                    }) {
                        let text = card.to_json()["text"].as_str().unwrap_or("").to_owned();
                        match automation.options.on_approval {
                            OnApproval::Stop => {
                                let _ = meter.decide(&self.0.store, &card.id, "stop");
                                self.cancel(&job.id).await?;
                                break ("spending_limit".into(), format!(
                                    "Stopped at the spending limit for paid models: {text}"
                                ));
                            }
                            OnApproval::Wait => {
                                if announced.insert(card.id.clone()) {
                                    self.automation_event(
                                        "automation.waiting",
                                        json!({
                                            "automation_id": automation.id,
                                            "run_id": run.id,
                                            "name": automation.name,
                                            "notify": automation.options.notify,
                                            "approval": "spending limit",
                                        }),
                                        Some(job.session_id.clone()),
                                    )
                                    .await;
                                }
                                continue;
                            }
                        }
                    }
                    let pending = self
                        .0
                        .approvals
                        .list(Some(&job.session_id))
                        .into_iter()
                        .find(|approval| approval.task_id == job.task_id);
                    let Some(approval) = pending else { continue };
                    let what = if approval.command.is_empty() {
                        approval.tool.clone()
                    } else {
                        truncate(&approval.command, 300).to_owned()
                    };
                    match automation.options.on_approval {
                        OnApproval::Stop => {
                            self.cancel(&job.id).await?;
                            break ("needs_approval".into(), format!(
                                "Stopped because it asked for approval: {what}. Automations stop at approval requests unless they are set to wait for you."
                            ));
                        }
                        OnApproval::Wait => {
                            if announced.insert(approval.id.clone()) {
                                self.automation_event(
                                    "automation.waiting",
                                    json!({
                                        "automation_id": automation.id,
                                        "run_id": run.id,
                                        "name": automation.name,
                                        "notify": automation.options.notify,
                                        "approval": what,
                                    }),
                                    Some(job.session_id.clone()),
                                )
                                .await;
                            }
                        }
                    }
                }
            }
        };
        let finished = self.job(&job.id)?.context("Job not found")?;
        run.status = if status == "cancelled" && finished.status == "completed" {
            "completed".into()
        } else {
            status
        };
        run.summary = truncate(&finished.summary, 4000).to_owned();
        run.usage = Some(json!(finished.usage));
        let mut notes = vec![];
        if !detail.is_empty() {
            notes.push(detail);
        }
        if run.status == "failed" && !finished.summary.is_empty() {
            notes.push(truncate(&finished.summary, 600).to_owned());
        }

        // A worktree with no edits is removed; one with edits is kept for
        // review in Tools › Worktrees.
        if let Some((record, path)) = &worktree {
            // A follow-up the user queued in the run's conversation (or a
            // plan-limit continuation) may use the worktree next: it is
            // only removed while nothing else runs or waits there, and the
            // reservations keep new work out while it is inspected and
            // removed.
            let idle = match (
                self.reserve_workspace(path),
                self.0.background.reserve_idle_workspace(path),
            ) {
                (Ok(tasks), Ok(background)) => Some((tasks, background)),
                _ => None,
            };
            let inspection = crate::worktrees::inspect(
                self.paths(),
                &workspace,
                &record.id,
                CancellationToken::new(),
            )
            .await;
            match inspection {
                Ok(_) if idle.is_none() => notes.push(format!(
                    "Another task or background process is using its worktree at {}, so it was kept.",
                    path.display()
                )),
                Ok(found) if found.head == record.base_commit && untouched(&found.status) => {
                    let sid = job.session_id.clone();
                    let main = workspace.clone();
                    store
                        .run(move |s| s.move_session_workspace(&sid, &main))
                        .await?;
                    self.discard_worktree(&workspace, &record.id, path).await;
                    if let Some(tree) = run.worktree.as_mut() {
                        tree["removed"] = json!(true);
                    }
                    notes.push("No files changed, so its temporary worktree was removed.".into());
                }
                Ok(_) => notes.push(format!(
                    "Its changes are on branch {} in a separate worktree. Review them or bring them back from Tools › Worktrees.",
                    record.branch
                )),
                Err(error) => notes.push(format!(
                    "The worktree at {} was kept: {error:#}",
                    path.display()
                )),
            }
        }
        run.detail = notes.join("\n");
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_automation_job(
        &self,
        automation: &Automation,
        run: &mut AutomationRun,
        folder: &Path,
        target: Option<&str>,
        model: Option<crate::config::ModelConfig>,
        model_label: &str,
        in_worktree: bool,
    ) -> Result<Job> {
        let store = self.0.store.clone();
        let (folder_owned, label, title) = (
            folder.to_owned(),
            model_label.to_owned(),
            format!("{} · automation", automation.name),
        );
        let (automation_id, run_id, target_owned) = (
            automation.id.clone(),
            run.id.clone(),
            target.map(str::to_owned),
        );
        let sid = store
            .run(move |s| {
                let session = s.create_session(&folder_owned, &label, &title)?;
                let sid = session["id"]
                    .as_str()
                    .context("Session ID missing")?
                    .to_owned();
                s.set_session_meta(&sid, keys::AUTOMATION_ID, &automation_id)?;
                s.set_session_meta(&sid, keys::AUTOMATION_RUN, &run_id)?;
                if let Some(target) = &target_owned {
                    s.set_session_meta(&sid, keys::EXECUTION_TARGET, target)?;
                }
                if in_worktree {
                    s.set_session_meta(&sid, keys::AUTOMATION_WORKTREE, "1")?;
                }
                Ok(sid)
            })
            .await?;
        run.session_id = Some(sid.clone());
        self.save_run(run.clone()).await?;
        let limit = match automation.options.permission {
            Permission::ReadOnly => Some(PermissionLevel::ReadOnly),
            Permission::Project => None,
        };
        let job = self
            .start_unattended(
                StartRequest {
                    workspace: folder.to_owned(),
                    task: automation.prompt.clone(),
                    session_id: Some(sid),
                    model,
                    mode: rules::task_mode(&automation.mode).into(),
                    queue: true,
                    images: Vec::new(),
                    web: false,
                },
                "",
                limit,
            )
            .await?;
        run.job_id = Some(job.id.clone());
        run.task_id = Some(job.task_id.clone());
        self.save_run(run.clone()).await?;
        Ok(job)
    }

    async fn discard_worktree(&self, workspace: &Path, id: &str, path: &Path) {
        if let Err(error) =
            crate::worktrees::dispose(self.paths(), workspace, id, CancellationToken::new()).await
        {
            tracing::warn!("Automation worktree {} was kept: {error:#}", path.display());
            return;
        }
        if let Err(error) = set_trust(self, path, false) {
            tracing::warn!("Could not update trusted projects: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_worktrees_ignore_ignored_files_and_shadow_state() {
        assert!(untouched(""));
        assert!(untouched("!! target/\n!! node_modules/"));
        assert!(untouched("?? .shadow/attachments/a.png"));
        assert!(!untouched("?? notes.txt"));
        assert!(!untouched(" M src/main.rs\n!! target/"));
        assert_eq!(job_outcome("completed"), "completed");
        assert_eq!(job_outcome("limit_reached"), "failed");
    }
}

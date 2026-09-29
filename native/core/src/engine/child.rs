//! Child jobs for subagents (`crate::subagents`). A child is an ordinary job
//! record with its own hidden session, run directly inside the parent's tool
//! call: it bypasses the workspace lane (the parent holds it) and the global
//! task slots (the parent holds one), and its cancellation token is a child
//! of the parent's.
use super::*;
use crate::subagents::{
    ApprovalRoute, ParentContext, SubagentHost, SubagentsConfig, ToolExtensions, ToolFilter,
};

/// What makes a job a child: shown with its approvals, bounded in depth,
/// optionally limited to some tools.
pub(crate) struct ChildLink {
    pub name: String,
    pub run_id: String,
    pub parent_session: String,
    /// The job whose tool call started this child. Its spending meter
    /// counts the child's paid model requests too.
    pub parent_job: String,
    pub depth: usize,
    pub filter: Option<Arc<ToolFilter>>,
}

pub(crate) struct ChildSpec {
    pub link: ChildLink,
    pub workspace: PathBuf,
    pub prompt: String,
    pub mode: String,
    /// Fully prepared: model, permissions, step limit and trust.
    pub config: Config,
    pub system_context: String,
    pub cancel: CancellationToken,
    pub web: bool,
    pub title: String,
}

impl Engine {
    /// Run one child to completion and return its final job record.
    /// `started` runs once the job and its session exist. Boxed: a child
    /// runs the same loop whose tool call started it.
    pub(crate) fn run_child<'a>(
        &'a self,
        spec: ChildSpec,
        started: impl FnOnce(&Job) + Send + 'a,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Job>> + Send + 'a>> {
        Box::pin(self.run_child_inner(spec, started))
    }
    async fn run_child_inner(
        &self,
        spec: ChildSpec,
        started: impl FnOnce(&Job) + Send,
    ) -> Result<Job> {
        ensure!(
            !self.0.closing.load(Ordering::Acquire),
            "Application is shutting down"
        );
        ensure!(
            !spec.prompt.trim().is_empty() && spec.prompt.len() <= 128_000,
            "A subagent needs a prompt of at most 128000 bytes"
        );
        let workspace = Arc::new(Workspace::open(&spec.workspace)?);
        ensure!(
            spec.config.is_trusted(&workspace.path),
            "Trust this project before starting subagents"
        );
        ensure!(
            matches!(
                crate::runtime::Runtime::for_model(&spec.config.model),
                crate::runtime::Runtime::Local
            ),
            "Subagents run on ShadowCode's own loop"
        );
        spec.config.validate()?;
        // Paid requests count toward the task the user started.
        let spend = self
            .running(&spec.link.parent_job)?
            .and_then(|parent| parent.spend.clone());
        let (job, running) = {
            let mut queues = self
                .0
                .queues
                .lock()
                .map_err(|_| anyhow!("Task queue lock poisoned"))?;
            ensure!(
                !self.0.closing.load(Ordering::Acquire),
                "Application is shutting down"
            );
            ensure!(!queues.manual.contains_key(&workspace.path), "Wait for the manual operation in this workspace to finish before starting a subagent");
            let store = &self.0.store;
            let session =
                store.create_session(&workspace.path, &spec.config.model.default, &spec.title)?;
            let sid = session["id"]
                .as_str()
                .context("Session missing ID")?
                .to_owned();
            use crate::store::keys;
            store.set_session_meta(&sid, keys::SUBAGENT_PARENT, &spec.link.parent_session)?;
            store.set_session_meta(&sid, keys::SUBAGENT_RUN, &spec.link.run_id)?;
            store.set_session_meta(&sid, keys::SUBAGENT_AGENT, &spec.link.name)?;
            let job = Job {
                id: crate::id(),
                workspace: workspace.path.clone(),
                session_id: sid.clone(),
                task_id: crate::id(),
                task: spec.prompt,
                web: spec.web,
                // Never queued: it starts inside the parent's tool call.
                status: "running".into(),
                mode: spec.mode,
                model: spec.config.model.name.clone(),
                started_at: crate::now(),
                event_cursor: store.event_cursor(&sid)?,
                ..Default::default()
            };
            store.create_job(&json!(job))?;
            let running = Arc::new(Running {
                clock: crate::timing::Clock::default(),
                record: Mutex::new(job.clone()),
                config: spec.config,
                system_context: Some(spec.system_context),
                command: None,
                workspace,
                cancel: spec.cancel,
                finished: AtomicBool::new(false),
                done: Notify::new(),
                steer: steering::SteerControl::default(),
                turn_plan: Default::default(),
                turn: Default::default(),
                spend,
                child: Some(spec.link),
                local_waiting: AtomicBool::new(false),
                local_admitted: AtomicBool::new(false),
            });
            queues.jobs.insert(job.id.clone(), running.clone());
            (job, running)
        };
        started(&job);
        let outcome = std::panic::AssertUnwindSafe(self.run(&running))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(anyhow!("The subagent worker panicked.")));
        let plan = outcome
            .as_ref()
            .map(|(_, plan)| plan.clone())
            .unwrap_or_else(|_| json!({"steps":[]}));
        let finished = self.finish(&running, outcome.map(|(summary, _)| summary), plan);
        if let Ok(mut queues) = self.0.queues.lock() {
            queues.jobs.remove(&job.id);
        }
        if let Err(error) = finished {
            self.finish_after_failed_save(&running, &error);
        }
        running.snapshot()
    }

    /// Add a finished child's tokens and cost to its parent job, so the
    /// parent task and its conversation show what the whole task used. The
    /// child's turns were priced when they ran; they are added as they are.
    /// The child's own job keeps its own usage too.
    pub(crate) fn add_child_usage(
        &self,
        parent_job: &str,
        events: &TaskEvents,
        child: &Usage,
        estimated: bool,
    ) -> Result<()> {
        if child.turns == 0 && child.total_tokens == 0 {
            return Ok(());
        }
        let Some(parent) = self.running(parent_job)? else {
            return Ok(());
        };
        let (total, session_id, task_id) = {
            let mut record = parent
                .record
                .lock()
                .map_err(|_| anyhow!("Job lock poisoned"))?;
            record.usage.add(child);
            record.usage_is_estimated |= estimated;
            self.0.store.save_job(&json!(*record))?;
            (
                record.usage.clone(),
                record.session_id.clone(),
                record.task_id.clone(),
            )
        };
        let session = crate::usage::session_total(&self.0.store, &session_id, &task_id, &total)?;
        events.emit(
            "usage.updated",
            crate::usage::event(child, &total, &session, "subagent"),
        )?;
        Ok(())
    }

    /// Subagents, skills, nested guidance and child limits for one job's tools.
    pub(super) fn tool_extensions(
        &self,
        running: &Running,
        job: &Job,
        events: &TaskEvents,
    ) -> ToolExtensions {
        let native = running.command.is_none();
        let depth = running.child.as_ref().map_or(0, |c| c.depth);
        let settings = SubagentsConfig::from_config(&running.config);
        let host = (native && settings.enabled && depth < settings.max_depth)
            .then(|| {
                SubagentHost::new(
                    self.clone(),
                    ParentContext {
                        job_id: job.id.clone(),
                        session_id: job.session_id.clone(),
                        task_id: job.task_id.clone(),
                        workspace: running.workspace.path.clone(),
                        config: running.config.clone(),
                        cancel: running.cancel.clone(),
                        events: events.clone(),
                        depth,
                    },
                    settings,
                )
                .ok()
                .map(Arc::new)
            })
            .flatten();
        let book = native.then(|| {
            Arc::new(crate::rulebook::Book::load(
                &self.0.paths,
                Some(&running.workspace.path),
            ))
        });
        ToolExtensions {
            host,
            filter: running.child.as_ref().and_then(|c| c.filter.clone()),
            approval: running.child.as_ref().map(|c| ApprovalRoute {
                session_id: c.parent_session.clone(),
                label: c.name.clone(),
            }),
            skills: Arc::new(
                book.as_ref()
                    .map(|book| book.model_skills(&running.workspace))
                    .unwrap_or_default(),
            ),
            guidance: native.then(|| Arc::new(crate::instructions::NestedGuidance::default())),
            rulebook: book,
        }
    }

    /// `@agent-name` in a top-level prompt starts that subagent before the
    /// model's first turn, like an explicitly named file is read first.
    pub(super) async fn mention_preflight(
        &self,
        running: &Running,
        job: &Job,
        tools: &ToolExecutor,
        schemas: &[Value],
        messages: &mut Vec<Value>,
    ) -> Result<()> {
        if running.child.is_some()
            || !schemas
                .iter()
                .any(|s| s["function"]["name"] == "spawn_agent")
        {
            return Ok(());
        }
        let Some(host) = tools.extensions().host.clone() else {
            return Ok(());
        };
        let Some((definition, prompt)) = crate::agents::mention(&job.task, host.catalog()) else {
            return Ok(());
        };
        let call = crate::models::ToolCall {
            id: crate::id(),
            name: "spawn_agent".into(),
            arguments: json!({
                "agent": definition.name,
                "prompt": prompt,
                "description": format!("@{} request", definition.name),
            }),
        };
        messages.push(json!({
            "role":"assistant",
            "content":format!("Starting the @{} subagent you asked for.", definition.name),
            "tool_calls":[{"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments.to_string()}}]
        }));
        self.0.store.save_messages(&job.id, messages)?;
        let result = tools.execute(call.clone()).await?;
        messages.push(result.message(
            &call.name,
            (running.config.model.context_limit * 2).min(running.config.agent.max_output_bytes),
        ));
        self.0.store.save_messages(&job.id, messages)?;
        Ok(())
    }
}

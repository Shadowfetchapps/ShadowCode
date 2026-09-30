//! "Resume at 3:40 PM" after a plan limit (`crate::resume`): scheduling,
//! cancelling, and the scheduler step that starts the continuation on the
//! same model. It runs with the automation scheduler (desktop and `serve`).
use super::*;
use crate::resume::{self as rules, Resume};

impl Engine {
    fn resume_event(&self, resume: &Resume, kind: &str, mut payload: Value) -> Result<()> {
        payload["resume_id"] = json!(resume.id);
        payload["at"] = json!(resume.at);
        payload["target"] = json!(resume.target);
        payload["label"] = json!(resume.label);
        // A continuation started from the window keeps the task's mode (a
        // Plan or Ask task stays read-only) and web access.
        payload["mode"] = json!(resume.mode);
        payload["web"] = json!(resume.web);
        payload["job_id"] = payload
            .get("job_id")
            .cloned()
            .unwrap_or(json!(resume.job_id));
        self.record_event(&resume.session_id, Some(&resume.task_id), kind, &payload)
    }

    /// Schedule the conversation of `job_id` (stopped at a plan limit) to
    /// continue on the same model when the limit resets. Replaces the
    /// conversation's earlier schedule.
    pub fn schedule_resume(&self, job_id: &str, handoff_consent: bool) -> Result<Resume> {
        let job = self.job(job_id)?.context("Job not found")?;
        ensure!(
            job.status == "limit_reached",
            "Only a task stopped at a plan limit can be resumed later"
        );
        let limit = job
            .result
            .as_ref()
            .map(|r| r["limit_reached"].clone())
            .unwrap_or_default();
        let at = limit["resets_at"]
            .as_f64()
            .filter(|at| *at > crate::now())
            .context("The vendor did not say when the limit resets, so a resume can't be scheduled. Try again later, or continue on another model.")?;
        let routing = job.routing.clone().unwrap_or_default();
        ensure!(
            !routing.model_id.is_empty(),
            "This task's model is not known, so it can't be resumed on the same model"
        );
        let label = limit["vendor"]
            .as_str()
            .and_then(crate::cli_agent::Vendor::parse)
            .or_else(|| crate::cli_agent::Vendor::from_provider(&routing.provider))
            .map(|v| v.product_label().to_owned())
            .unwrap_or_else(|| routing.model_name.clone());
        let resume = Resume {
            id: crate::id(),
            session_id: job.session_id.clone(),
            workspace: job.workspace.clone(),
            job_id: job.id.clone(),
            task_id: job.task_id.clone(),
            target: routing.model_id,
            label,
            task: job.task.clone(),
            mode: job.mode.clone(),
            web: job.web,
            at,
            created_at: crate::now(),
            handoff_consent,
        };
        rules::save(&self.0.store, &resume)?;
        self.resume_event(
            &resume,
            "resume.scheduled",
            json!({"scheduler": self.automations_scheduled()}),
        )?;
        Ok(resume)
    }

    /// Cancel the conversation's scheduled resume, if any.
    pub fn cancel_resume(&self, session_id: &str) -> Result<Option<Resume>> {
        let resume = rules::take(&self.0.store, session_id)?;
        if let Some(resume) = &resume {
            self.resume_event(resume, "resume.cancelled", json!({}))?;
        }
        Ok(resume)
    }

    /// Start every resume whose time has come (one scheduler tick). A time
    /// missed by more than [`rules::LATE_SECS`] is reported instead.
    pub(super) async fn resume_tick(&self, now: f64) -> Result<()> {
        let store = self.0.store.clone();
        let due = store.run(move |s| rules::take_due(s, now)).await?;
        for resume in due {
            if now - resume.at > rules::LATE_SECS {
                self.resume_event(&resume, "resume.missed", json!({}))?;
                continue;
            }
            if let Err(error) = self.start_resume(&resume).await {
                let consent = error
                    .downcast_ref::<crate::cli_agent::handoff::ConsentRequired>()
                    .is_some();
                self.resume_event(
                    &resume,
                    if consent {
                        "resume.needs_consent"
                    } else {
                        "resume.failed"
                    },
                    json!({"reason": format!("{error:#}"), "task": resume.continuation()}),
                )?;
            }
        }
        Ok(())
    }

    async fn start_resume(&self, resume: &Resume) -> Result<()> {
        let config = Config::load(&self.0.paths, Some(&resume.workspace))?;
        // The same model, never another one: when it cannot run, the
        // conversation says so (`resume.failed`).
        let model = crate::model_registry::resolve(&self.0.store, &resume.target, &config.model)
            .with_context(|| format!("{} can't be used right now", resume.label))?;
        let job = self
            .start_consented_owned(
                StartRequest {
                    workspace: resume.workspace.clone(),
                    task: resume.continuation(),
                    session_id: Some(resume.session_id.clone()),
                    model: Some(model),
                    mode: resume.mode.clone(),
                    queue: true,
                    images: Vec::new(),
                    web: resume.web,
                },
                "coder",
                None,
                None,
                resume.handoff_consent,
            )
            .await?;
        self.0.store.set_session_meta(
            &resume.session_id,
            keys::EXECUTION_TARGET,
            &resume.target,
        )?;
        self.resume_event(resume, "resume.started", json!({"job_id": job.id}))
    }
}

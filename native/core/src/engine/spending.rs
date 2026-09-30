//! Spending limits in the task loop (`crate::spending`): paid model requests
//! are counted as they are priced, and before each model turn the task
//! checks its limits and, when one is reached, waits for the user's answer.
use super::*;
use crate::spending::{Check, Meter};

/// How often a task waiting at a limit looks again, so a limit raised in
/// Settings, raised by another task, or reset at midnight lets it go on.
const RECHECK: Duration = Duration::from_secs(3);

impl Engine {
    /// Count one priced model request toward the task's and the day's
    /// paid spending.
    pub(super) fn record_spend(&self, running: &Running, turn: &Usage) -> Result<()> {
        match &running.spend {
            Some(meter) => meter.record(&self.0.store, &running.config.model, turn),
            None => Ok(()),
        }
    }

    /// The limits in force now: Settings can change them while a task runs.
    fn spending_config(&self, running: &Running) -> crate::spending::SpendingConfig {
        Config::load(&self.0.paths, Some(&running.workspace.path))
            .map(|config| config.spending)
            .unwrap_or_else(|_| running.config.spending.clone())
    }

    /// Before a model turn on a paid model: go on when no limit is reached;
    /// otherwise show the limit card (once, in the task the user started)
    /// and wait until it is answered, the limit no longer applies, or the
    /// task is cancelled.
    pub(super) async fn spend_gate(&self, running: &Running) -> Result<()> {
        let Some(meter) = running.spend.clone() else {
            return Ok(());
        };
        if !crate::spending::is_paid(&running.config.model) {
            return Ok(());
        }
        loop {
            // Registered before looking, so an answer given in between is
            // not missed.
            let answered = meter.notified();
            let config = self.spending_config(running);
            let now = crate::now();
            let (kind, limit) = match meter.check(&self.0.store, &config, now)? {
                Check::Clear => {
                    meter.lift("limit_changed")?;
                    return Ok(());
                }
                Check::Reached(kind, limit) => (kind, limit),
            };
            if meter.stopped().is_some() {
                bail!("{}", crate::spending::stopped_summary(kind));
            }
            if meter.is_unattended() {
                bail!("{}", crate::spending::unattended_summary(kind, limit));
            }
            meter.ask(&self.0.store, &config, kind, limit, now)?;
            tokio::select! {
                _ = answered => {}
                _ = running.cancel.cancelled() => {
                    bail!("Task cancelled at the spending limit. Completed changes remain checkpointed.");
                }
                _ = tokio::time::sleep(RECHECK) => {}
            }
        }
    }

    fn meter_of(&self, job_id: &str) -> Result<Arc<Meter>> {
        self.running(job_id)?
            .context("This task has already finished")?
            .spend
            .clone()
            .context("This task has no spending limit")
    }

    /// The user's answer to a task's limit card: `continue` raises the
    /// limit one step and the task goes on; `stop` ends the task.
    pub fn decide_spending(&self, job_id: &str, prompt_id: &str, action: &str) -> Result<Value> {
        let meter = self.meter_of(job_id)?;
        let answer = meter.decide(&self.0.store, prompt_id, action)?;
        if action == "stop" {
            self.request_cancel(job_id)?;
        }
        Ok(answer)
    }

    /// Limit cards waiting for an answer, across running tasks.
    pub fn spending_waiting(&self) -> Result<Vec<Value>> {
        let queues = self
            .0
            .queues
            .lock()
            .map_err(|_| anyhow!("Task queue lock poisoned"))?;
        let mut waiting = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for running in queues.jobs.values() {
            let Some(meter) = &running.spend else {
                continue;
            };
            if seen.insert(meter.job_id().to_owned()) {
                waiting.extend(meter.waiting_card());
            }
        }
        Ok(waiting)
    }
}

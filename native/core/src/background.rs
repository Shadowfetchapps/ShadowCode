//! Managed project servers and watchers. Only live handles are cancelled;
//! persisted PIDs are informational and are never used to signal a process.
use crate::{
    config::Config,
    permissions::{self, Decision},
    process::{self, ProcessMonitor, ProcessSpec},
    store::Store,
    workspace::Workspace,
};
use anyhow::{anyhow, ensure, Context, Result};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BackgroundTask {
    pub id: String,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub status: String,
    pub pid: u32,
    pub started_at: f64,
    pub ended_at: Option<f64>,
    pub exit_code: Option<i32>,
    pub output: String,
    pub error: String,
    pub truncated: bool,
    pub session_id: Option<String>,
    pub origin_task_id: Option<String>,
}
struct Running {
    record: Mutex<BackgroundTask>,
    monitor: ProcessMonitor,
    cancel: CancellationToken,
    finished: AtomicBool,
    done: Notify,
}
impl Running {
    fn snapshot(&self) -> Result<BackgroundTask> {
        let mut task = self
            .record
            .lock()
            .map_err(|_| anyhow!("Background task lock poisoned"))?
            .clone();
        let live = self.monitor.snapshot()?;
        task.pid = live.pid;
        task.output = live.output;
        task.truncated |= live.truncated;
        if task.status == "STARTING" && task.pid != 0 {
            task.status = "RUNNING".into();
        }
        Ok(task)
    }
    async fn wait(&self) {
        loop {
            let done = self.done.notified();
            if self.finished.load(Ordering::Acquire) {
                break;
            }
            done.await;
        }
    }
}
#[derive(Default)]
struct State {
    closing: bool,
    reserved_workspaces: HashSet<PathBuf>,
    tasks: HashMap<String, Arc<Running>>,
    workers: Vec<tokio::task::JoinHandle<()>>,
}
pub struct BackgroundManager {
    store: Arc<Store>,
    state: Mutex<State>,
    profile_lock: Arc<crate::paths::ProfileLock>,
}
impl Drop for BackgroundManager {
    fn drop(&mut self) {
        let _ = self.begin_shutdown();
    }
}
pub(crate) struct IdleWorkspace {
    manager: Arc<BackgroundManager>,
    path: PathBuf,
}
impl Drop for IdleWorkspace {
    fn drop(&mut self) {
        if let Ok(mut state) = self.manager.state.lock() {
            state.reserved_workspaces.remove(&self.path);
        }
    }
}
impl BackgroundManager {
    pub(crate) fn reserve_idle_workspace(self: &Arc<Self>, path: &Path) -> Result<IdleWorkspace> {
        let path = crate::workspace::reservation_path(path)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Background registry lock poisoned"))?;
        ensure!(
            !state
                .tasks
                .values()
                .any(|task| !task.finished.load(Ordering::Acquire)
                    && task
                        .record
                        .lock()
                        .map(|record| Path::new(&record.cwd) == path)
                        .unwrap_or(true)),
            "Stop background processes before removing this worktree"
        );
        ensure!(
            state.reserved_workspaces.insert(path.clone()),
            "This workspace is being removed"
        );
        Ok(IdleWorkspace {
            manager: self.clone(),
            path,
        })
    }
    pub(crate) fn new(store: Arc<Store>, profile_lock: Arc<crate::paths::ProfileLock>) -> Self {
        Self {
            store,
            state: Mutex::new(State::default()),
            profile_lock,
        }
    }
    pub fn start(
        &self,
        workspace: &Path,
        config: &Config,
        session_id: Option<String>,
        name: &str,
        command: &str,
    ) -> Result<BackgroundTask> {
        self.start_inner(workspace, config, session_id, name, command, None)
    }
    pub(crate) fn start_for_task(
        &self,
        workspace: &Path,
        config: &Config,
        events: &crate::events::TaskEvents,
        name: &str,
        command: &str,
    ) -> Result<BackgroundTask> {
        self.start_inner(
            workspace,
            config,
            Some(events.session_id.clone()),
            name,
            command,
            Some(events.task_id.clone()),
        )
    }
    fn start_inner(
        &self,
        workspace: &Path,
        config: &Config,
        session_id: Option<String>,
        name: &str,
        command: &str,
        origin_task_id: Option<String>,
    ) -> Result<BackgroundTask> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 80,
            "Process name must contain 1–80 bytes"
        );
        ensure!(
            !command.trim().is_empty() && command.len() <= 64000,
            "Command must contain 1–64000 bytes"
        );
        let workspace = Workspace::open(workspace)?;
        ensure!(
            config.is_trusted(&workspace.path),
            "Trust this project before running background processes"
        );
        if let Decision::Deny(reason) =
            permissions::check(&config.permissions, "exec", &json!({"command":command}))
        {
            anyhow::bail!(reason);
        }
        // Starting from this API is an explicit user command, like Terminal Run.
        // Agent-originated process tools must use their scoped approval hub.
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Background registry lock poisoned"))?;
        ensure!(!state.closing, "Application is shutting down");
        ensure!(
            !state.reserved_workspaces.contains(&workspace.path),
            "This workspace is being removed"
        );
        state
            .tasks
            .retain(|_, task| !task.finished.load(Ordering::Acquire));
        state.workers.retain(|worker| !worker.is_finished());
        ensure!(
            state.tasks.len() < 16,
            "At most 16 background processes may be active"
        );
        let cwd = workspace.path.to_string_lossy().into_owned();
        let active: Vec<_> = state
            .tasks
            .values()
            .map(|task| task.snapshot())
            .collect::<Result<_>>()?;
        ensure!(
            active.iter().filter(|task| task.cwd == cwd).count() < 4,
            "At most four background processes may run in one project"
        );
        ensure!(
            !active
                .iter()
                .any(|task| task.cwd == cwd && task.name == name.trim()),
            "A process with this name is already running in this project"
        );
        let task = BackgroundTask {
            id: crate::id(),
            name: name.trim().into(),
            command: command.into(),
            cwd,
            status: "STARTING".into(),
            started_at: crate::now(),
            session_id,
            origin_task_id,
            ..Default::default()
        };
        self.store.save_background_event(
            &task,
            "background.started",
            &json!({"id":task.id,"name":task.name,"command":task.command,"workspace":task.cwd}),
        )?;
        let running = Arc::new(Running {
            record: Mutex::new(task.clone()),
            monitor: ProcessMonitor::default(),
            cancel: CancellationToken::new(),
            finished: AtomicBool::new(false),
            done: Notify::new(),
        });
        state.tasks.insert(task.id.clone(), running.clone());
        let id = task.id.clone();
        let store = self.store.clone();
        let profile_lock = self.profile_lock.clone();
        state.workers.push(tokio::spawn(async move {
            let _profile_lock = profile_lock;
            let process = std::panic::AssertUnwindSafe(Self::run(
                &store,
                &running,
                ProcessSpec::shell(
                    &task.command,
                    workspace.path.clone(),
                    Duration::from_secs(60),
                ),
            ))
            .catch_unwind()
            .await;
            if let Err(error) = match process {
                Ok(result) => result,
                Err(_) => Err(anyhow!("Background worker stopped unexpectedly")),
            } {
                running.cancel.cancel();
                if let Ok(mut record) = running.record.lock() {
                    record.status = "FAILED".into();
                    record.error = format!("{error:#}");
                    record.ended_at = Some(crate::now());
                }
                if let Ok(task) = running.snapshot() {
                    let _ = store.save_background(&task);
                }
            }
            running.finished.store(true, Ordering::Release);
            running.done.notify_waiters();
        }));
        state
            .tasks
            .get(&id)
            .context("Process was not registered")?
            .snapshot()
    }
    async fn run(store: &Store, running: &Running, spec: ProcessSpec) -> Result<()> {
        let process =
            process::run_background(spec, running.cancel.clone(), running.monitor.clone());
        tokio::pin!(process);
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        let mut revision = 0;
        let result = loop {
            tokio::select! {
                result=&mut process=>break result,
                _=interval.tick()=>{
                    let next=running.monitor.snapshot()?.revision;
                    if next!=revision { store.save_background(&running.snapshot()?)?; revision=next; }
                }
            }
        };
        {
            let mut record = running
                .record
                .lock()
                .map_err(|_| anyhow!("Background task lock poisoned"))?;
            record.ended_at = Some(crate::now());
            match result {
                Ok(result) => {
                    record.status = if result.cancelled {
                        "CANCELLED"
                    } else if result.ok {
                        "COMPLETED"
                    } else {
                        "FAILED"
                    }
                    .into();
                    record.exit_code = Some(result.exit_code);
                    record.truncated = result.truncated;
                }
                Err(error) => {
                    record.status = if running.cancel.is_cancelled() {
                        "CANCELLED"
                    } else {
                        "FAILED"
                    }
                    .into();
                    record.error = format!("{error:#}");
                }
            }
        }
        let task = running.snapshot()?;
        store.save_background_event(&task,"background.completed",&json!({"id":task.id,"name":task.name,"workspace":task.cwd,"status":task.status,"exit_code":task.exit_code,"error":task.error,"truncated":task.truncated}))?;
        Ok(())
    }
    pub fn list(&self, workspace: &Path) -> Result<Vec<BackgroundTask>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Background registry lock poisoned"))?;
        let mut tasks = self.store.background_tasks(workspace)?;
        for task in &mut tasks {
            if let Some(running) = state.tasks.get(&task.id) {
                *task = running.snapshot()?;
            }
        }
        for running in state
            .tasks
            .values()
            .filter(|task| !task.finished.load(Ordering::Acquire))
        {
            let task = running.snapshot()?;
            if Path::new(&task.cwd) == workspace
                && !tasks.iter().any(|existing| existing.id == task.id)
            {
                tasks.push(task);
            }
        }
        tasks.sort_by(|a, b| {
            b.started_at
                .total_cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(tasks)
    }
    pub fn get(&self, id: &str) -> Result<BackgroundTask> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Background registry lock poisoned"))?;
        if let Some(running) = state.tasks.get(id) {
            return running.snapshot();
        }
        self.store
            .background_task(id)?
            .context("Background process not found")
    }
    pub async fn stop(&self, id: &str) -> Result<BackgroundTask> {
        let running = self
            .state
            .lock()
            .map_err(|_| anyhow!("Background registry lock poisoned"))?
            .tasks
            .get(id)
            .cloned();
        if let Some(running) = running {
            // Once the process is finished, Stop is idempotent and must not
            // rewrite COMPLETED/FAILED as CANCELLED.
            if !running.finished.load(Ordering::Acquire) {
                {
                    let mut record = running
                        .record
                        .lock()
                        .map_err(|_| anyhow!("Background task lock poisoned"))?;
                    if matches!(record.status.as_str(), "STARTING" | "RUNNING") {
                        record.status = "STOPPING".into();
                    }
                }
                running.cancel.cancel();
                running.wait().await;
            }
        }
        self.get(id)
    }
    pub fn begin_shutdown(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Background registry lock poisoned"))?;
        state.closing = true;
        for task in state.tasks.values() {
            task.cancel.cancel();
        }
        Ok(())
    }
    pub async fn wait_shutdown(&self) -> Result<()> {
        let tasks: Vec<_> = self
            .state
            .lock()
            .map_err(|_| anyhow!("Background registry lock poisoned"))?
            .tasks
            .values()
            .cloned()
            .collect();
        // Keep worker handles registered until cleanup is complete, so a timed
        // out shutdown can be retried without losing ownership of live work.
        for task in tasks {
            task.wait().await;
        }
        let workers = std::mem::take(
            &mut self
                .state
                .lock()
                .map_err(|_| anyhow!("Background registry lock poisoned"))?
                .workers,
        );
        for worker in workers {
            worker.await.context("Background worker did not finish")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod removal_tests {
    use super::*;
    #[tokio::test]
    async fn idle_workspace_reservation_blocks_new_processes_and_releases_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let paths = crate::paths::AppPaths::isolated(&root.path().join("profile")).unwrap();
        Config::patch(&paths, json!({"trusted_workspaces":[project]})).unwrap();
        let config = Config::load(&paths, Some(&project)).unwrap();
        let service = crate::service::Service::open(paths, Some(project.clone())).unwrap();
        let manager = service.engine.background();
        let reservation = manager.reserve_idle_workspace(&project).unwrap();
        assert!(manager
            .start(&project, &config, None, "blocked", "sleep 60")
            .unwrap_err()
            .to_string()
            .contains("being removed"));
        assert!(manager.list(&project).unwrap().is_empty());
        drop(reservation);
        let process = manager
            .start(&project, &config, None, "allowed", "sleep 60")
            .unwrap();
        assert!(manager.reserve_idle_workspace(&project).is_err());
        manager.stop(&process.id).await.unwrap();
        drop(manager.reserve_idle_workspace(&project).unwrap());
        service.engine.shutdown().await.unwrap();
    }
}

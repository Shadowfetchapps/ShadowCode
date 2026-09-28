//! Application commands shared by Tauri IPC and the optional loopback transport.
//!
//! `Service::dispatch` is a small router: it parses the request into a
//! [`Call`] and hands it to the route module that owns the path's family
//! (the segment after `/api/`). Each module under `service/` owns one domain
//! and answers every route in its families, ending with
//! `Err(call.unavailable())` for paths it does not know.
//!
//! Adding routes:
//! 1. Create `service/<domain>.rs` with `use super::*;` and an
//!    `impl Service { pub(super) async fn <domain>_routes(&self, call: &Arc<Call>) -> Result<Value> }`
//!    that matches on `(call.method.as_str(), call.path.as_str())` and, for
//!    `/api/<family>/<id>/…` routes, on `call.parts()`.
//! 2. Read bodies through a `#[derive(Default, Deserialize)] #[serde(default)]`
//!    struct of `Text` / `Flag` / `Loose<T>` fields with `call.body()?`; they
//!    read absent or mistyped fields the way the untyped API always did.
//! 3. Handlers that only do synchronous work (the database, config files,
//!    the workspace) run through `self.blocking(call, Self::handler)` so a
//!    slow write never stalls an async worker; handlers that await engine or
//!    process futures stay async.
//! 4. Declare the module below and add its families to the table in
//!    `dispatch`. Keep response shapes in `docs/API_CONTRACT_0.28.md`.
use crate::{
    checkpoint,
    config::{self, Config, ModelConfig, PermissionLevel},
    engine::{Engine, JobOwner, StartRequest, WorkspaceReservation},
    model_registry,
    models::{self, ModelClient},
    paths::AppPaths,
    permissions::{self, Decision},
    process::{self, ProcessSpec},
    routing,
    store::MilestoneSpec,
    tools::truncate,
    workspace::Workspace,
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Write,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
mod about;
mod accounts;
mod agents;
mod automations;
mod background;
mod call;
mod code_intel;
mod commands;
mod compare;
mod composer;
mod diagnostic_export;
mod extensions;
mod feed;
mod forge;
mod git;
mod goals;
#[cfg(unix)]
mod inspection;
mod issues;
mod jobs;
mod local_downloads;
mod memory;
mod model_catalog;
#[cfg(target_os = "linux")]
mod preview;
#[cfg(unix)]
mod remote;
mod review;
mod sandbox;
mod sessions;
mod settings;
mod terminals;
mod voice;
mod workspace;
mod worktree_tasks;
mod worktrees;
use call::{Call, Flag, Loose, Text};
pub use git::parse_hunks;

#[derive(Clone)]
struct Selection {
    generation: u64,
    workspace: PathBuf,
    session: Option<String>,
}
type DetectionCache = Arc<tokio::sync::Mutex<Option<(Instant, Vec<Value>)>>>;
struct ManualWorkspace {
    workspace: Workspace,
    reservation: WorkspaceReservation,
}
impl Deref for ManualWorkspace {
    type Target = Workspace;
    fn deref(&self) -> &Workspace {
        &self.workspace
    }
}
#[derive(Clone)]
pub struct Service {
    pub engine: Engine,
    selection: Arc<RwLock<Selection>>,
    detection: DetectionCache,
    diagnostic_exports: Arc<diagnostic_export::DiagnosticExports>,
    guardian: Arc<crate::guardian::Guardian>,
    remember_selection: bool,
    job_owner: Option<JobOwner>,
    /// This view's interactive terminals (a forked view starts with none).
    terminals: Arc<crate::terminal::Terminals>,
    /// Loopback proxies for the in-app preview, shared by every view.
    #[cfg(target_os = "linux")]
    previews: Arc<crate::preview::Previews>,
    /// Remote access and phone notifications (one per engine).
    #[cfg(unix)]
    remote: Arc<crate::remote::Manager>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub body: Value,
}
impl Service {
    pub fn open(paths: AppPaths, workspace: Option<PathBuf>) -> Result<Self> {
        let workspace = workspace
            .or_else(|| paths.remembered_workspace())
            .unwrap_or(std::env::current_dir()?);
        let workspace = Workspace::open(&workspace)?.path;
        #[cfg(unix)]
        let remote = Arc::new(crate::remote::Manager::new(paths.clone()));
        let engine = Engine::open(paths)?;
        let terminals = Arc::new(crate::terminal::Terminals::new(engine.notifier()));
        Ok(Self {
            engine,
            selection: Arc::new(RwLock::new(Selection {
                generation: 0,
                workspace,
                session: None,
            })),
            detection: Arc::new(tokio::sync::Mutex::new(None)),
            diagnostic_exports: Arc::default(),
            guardian: Arc::new(crate::guardian::Guardian::default()),
            remember_selection: true,
            job_owner: None,
            terminals,
            #[cfg(target_os = "linux")]
            previews: Arc::default(),
            #[cfg(unix)]
            remote,
        })
    }
    /// A transport client shares the engine, but has its own navigation state.
    /// Its requests must never activate a different project in the desktop.
    pub fn fork_selection(&self, workspace: PathBuf, session: Option<String>) -> Result<Self> {
        let workspace = Workspace::open(&workspace)?.path;
        if let Some(id) = &session {
            ensure!(
                self.engine
                    .store()
                    .session(id)?
                    .context("Session not found")?["workspace"]
                    .as_str()
                    == workspace.to_str(),
                "Session belongs to a different workspace"
            );
        }
        Ok(Self {
            engine: self.engine.clone(),
            selection: Arc::new(RwLock::new(Selection {
                workspace,
                session,
                generation: 0,
            })),
            detection: self.detection.clone(),
            diagnostic_exports: self.diagnostic_exports.clone(),
            guardian: self.guardian.clone(),
            remember_selection: false,
            job_owner: None,
            // An attached window's terminals live as long as its view.
            terminals: Arc::new(crate::terminal::Terminals::new(self.engine.notifier())),
            #[cfg(target_os = "linux")]
            previews: self.previews.clone(),
            #[cfg(unix)]
            remote: self.remote.clone(),
        })
    }
    /// Remote access (web interface, pairing, phone notifications).
    #[cfg(unix)]
    pub fn remote(&self) -> &Arc<crate::remote::Manager> {
        &self.remote
    }
    pub(crate) fn with_job_owner(mut self, owner: JobOwner) -> Self {
        self.job_owner = Some(owner);
        self
    }
    pub fn workspace(&self) -> Result<PathBuf> {
        Ok(self
            .selection
            .read()
            .map_err(|_| anyhow::anyhow!("Project selection lock poisoned"))?
            .workspace
            .clone())
    }
    fn config(&self) -> Result<Config> {
        let config = Config::load(self.engine.paths(), Some(&self.workspace()?))?;
        self.engine
            .vendors()
            .configure(&config.cli_agents, config.offline());
        Ok(config)
    }
    fn snapshot_selection(&self) -> Result<Selection> {
        Ok(self
            .selection
            .read()
            .map_err(|_| anyhow::anyhow!("Project selection lock poisoned"))?
            .clone())
    }
    fn select(&self, path: &Path, session: Option<String>) -> Result<()> {
        self.select_if(path, session, None)
    }
    fn select_if(&self, path: &Path, session: Option<String>, expected: Option<u64>) -> Result<()> {
        let workspace = Workspace::open(path)?.path;
        let mut selection = self
            .selection
            .write()
            .map_err(|_| anyhow::anyhow!("Project selection lock poisoned"))?;
        if expected.is_some_and(|generation| generation != selection.generation) {
            return Ok(());
        }
        // A Compare lane's, worktree task's or automation run's worktree is
        // temporary: it is selected while its conversation is open but never
        // becomes a project or the relaunch folder, which would go stale once
        // the comparison is kept, the task applied or the run's checkout
        // removed.
        let lane = match &session {
            Some(id) => {
                let store = self.engine.store();
                crate::compare::session_tags(&store, id)?.0.is_some()
                    || crate::worktree_tasks::session_task(&store, id)?.is_some()
                    || store
                        .session_meta(id, crate::store::keys::AUTOMATION_WORKTREE)?
                        .is_some()
            }
            None => false,
        };
        if !lane {
            if self.remember_selection {
                self.engine.paths().remember_workspace(&workspace)?;
            }
            self.engine.store().touch_project(&workspace)?;
        }
        *selection = Selection {
            workspace,
            session,
            generation: selection.generation.wrapping_add(1),
        };
        Ok(())
    }
    pub async fn detected(&self, refresh: bool) -> Vec<Value> {
        let mut cache = self.detection.lock().await;
        if !refresh {
            if let Some((time, providers)) = &*cache {
                if time.elapsed() < Duration::from_secs(30) {
                    return providers.clone();
                }
            }
        }
        let providers = models::detect().await;
        *cache = Some((Instant::now(), providers.clone()));
        providers
    }
    pub async fn dispatch(&self, request: Request) -> Result<Value> {
        let call = Arc::new(Call::parse(request)?);
        self.check_worktree_project(&call)?;
        match call.family() {
            "compare" | "compares" => self.compare(&call).await,
            "agents" | "subagents" => self.blocking(&call, Self::agent_routes).await,
            "worktrees" | "parallel" => self.worktree_routes(&call).await,
            "worktree-tasks" => self.worktree_task_routes(&call).await,
            "sandbox" => self.sandbox_routes(&call).await,
            "sessions" | "projects" | "events" | "resolve" => {
                self.blocking(&call, Self::session_routes).await
            }
            "jobs" | "run" | "approvals" | "checkpoints" => self.job_routes(&call).await,
            "goals" => self.goal_routes(&call).await,
            "automations" => self.automation_routes(&call).await,
            "issues" => self.issue_routes(&call).await,
            "review" => self.review_routes(&call).await,
            "feed" => self.feed_routes(&call).await,
            "terminals" => self.terminal_routes(&call).await,
            #[cfg(unix)]
            "remote" => self.remote_routes(&call).await,
            "git" => self.forge_routes(&call).await,
            "background" => self.background_routes(&call).await,
            #[cfg(target_os = "linux")]
            "preview" => self.preview_routes(&call).await,
            "code-intel" => self.code_intel_routes(&call).await,
            "voice" => self.voice_routes(&call).await,
            "workspace" => self.workspace_routes(&call).await,
            "config" | "routing" | "onboarding" | "health" | "version" | "doctor"
            | "diagnostic-exports" | "guardian" => self.settings_routes(&call).await,
            "about" | "updates" => self.about_routes(&call).await,
            "accounts" | "cli-agents" | "openrouter" | "allowance" => {
                self.account_routes(&call).await
            }
            "providers" | "models" | "picker" | "local-models" => self.model_routes(&call).await,
            "plugins" | "mcp" | "hooks" | "sqlite" => self.extension_routes(&call).await,
            "commands" | "memory" => match (call.method.as_str(), call.path.as_str()) {
                ("GET", "/api/commands") => self.command_catalog(),
                ("POST", "/api/commands/run") => self.run_command(&call.body).await,
                ("POST", "/api/memory") => {
                    self.blocking(&call, |service, call| service.memory(&call.body))
                        .await
                }
                _ => Err(call.unavailable()),
            },
            _ => Err(call.unavailable()),
        }
    }
    /// Run a synchronous handler on the blocking pool. Database, config and
    /// workspace I/O may wait on the store lock or on `fsync`; that must not
    /// stall the async workers that drive running jobs. A panic in the
    /// handler resumes in the caller, as it did before the handoff.
    async fn blocking<T: Send + 'static>(
        &self,
        call: &Arc<Call>,
        handler: fn(&Service, &Call) -> Result<T>,
    ) -> Result<T> {
        let (service, call) = (self.clone(), call.clone());
        match tokio::task::spawn_blocking(move || handler(&service, &call)).await {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => Err(anyhow::anyhow!("Application command stopped: {error}")),
        }
    }
    fn current_session(&self) -> Result<Option<String>> {
        Ok(self
            .selection
            .read()
            .map_err(|_| anyhow::anyhow!("Project lock poisoned"))?
            .session
            .clone())
    }
    /// Attachments are user input written as new, uniquely named files under
    /// .shadow/attachments (size/format limits apply). Reading them is not a
    /// project change, so read-only projects accept them; trust is required.
    fn attachment_workspace(&self) -> Result<Workspace> {
        let workspace = Workspace::open(&self.workspace()?)?;
        let cfg = Config::load(self.engine.paths(), Some(&workspace.path))?;
        ensure!(
            cfg.is_trusted(&workspace.path),
            "Trust this project before attaching files"
        );
        Ok(workspace)
    }
    fn mutable_workspace(&self) -> Result<ManualWorkspace> {
        self.mutable_workspace_at(&self.workspace()?)
    }
    fn mutable_workspace_at(&self, path: &Path) -> Result<ManualWorkspace> {
        let workspace = Workspace::open(path)?;
        let cfg = Config::load(self.engine.paths(), Some(&workspace.path))?;
        ensure!(
            cfg.permissions.level != PermissionLevel::ReadOnly,
            "This project is in read-only mode"
        );
        ensure!(
            cfg.is_trusted(&workspace.path),
            "Trust this project before changing files or running commands"
        );
        let reservation = self.engine.reserve_workspace(&workspace.path)?;
        Ok(ManualWorkspace {
            workspace,
            reservation,
        })
    }
}
fn active(job: &Value) -> bool {
    matches!(
        job["status"].as_str(),
        Some("queued" | "running" | "paused" | "cancelling")
    )
}
fn query_limit(query: &HashMap<String, String>, default: usize, max: usize) -> usize {
    query
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(1, max)
}
fn expand_path(path: &str) -> Result<PathBuf> {
    ensure!(!path.is_empty(), "Project path required");
    Ok(if path == "~" {
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
    } else if let Some(tail) = path.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(tail)
    } else {
        PathBuf::from(path)
    })
}

//! Subagents: child jobs the native loop starts with the `spawn_agent` tool.
//!
//! A child has its own conversation (a session hidden from the sidebar but
//! viewable), its own context, an agent definition (`crate::agents`), and an
//! optional model override. Read-only children work in the parent's project
//! with read-only permissions. Write children work in their own managed Git
//! worktree started from the parent's current files; their diff comes back
//! to the parent, which applies it with `apply_agent_changes` (an ordinary,
//! checkpointed, approval-checked patch). The worktree is removed afterwards.
//!
//! Children run in parallel up to `subagents.max_parallel`, cannot start
//! their own children unless `subagents.max_depth` allows it, stop when the
//! parent is cancelled, and ask their approvals in the parent conversation
//! with the child's name.
//!
//! A child may run on a vendor CLI (a project role, an agent definition's
//! `model: cli:claude:…`, or the call's `model`). It is then a normal vendor
//! job in its own conversation (`Engine::run_cli_agent`): the vendor's
//! approvals are asked in the parent conversation, stopping the parent stops
//! it, its usage counts toward the parent, and a write child still edits its
//! own worktree and returns a diff. Project roles (`crate::roles`) choose the
//! model of `explore`, `plan`, `review` and write children, and run the
//! stages of a Plan → Implement → Review task (`Self::run_role`).
use crate::{
    agents::{self, AgentCatalog, AgentDefinition},
    cli_agent::{handoff, Vendor},
    compare,
    config::{Config, ModelConfig, PermissionLevel},
    engine::{ChildLink, ChildSpec, Engine, Job},
    events::TaskEvents,
    instructions::NestedGuidance,
    roles::{self, Role},
    store::Store,
    tools::truncate,
    workspace::Workspace,
    worktrees,
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Diff text handed back to the parent model; the full patch stays on disk.
const DIFF_FOR_MODEL: usize = 16_000;
const SUMMARY_FOR_MODEL: usize = 6_000;
const MAX_BATCH: usize = 8;
const INDEXED: usize = 200;
const SKILL_BYTES: usize = 32_000;

/// `subagents:` in config.yaml.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SubagentsConfig {
    /// Offer `spawn_agent` to the native loop.
    pub enabled: bool,
    /// Children of one task that may run at the same time (1–8).
    pub max_parallel: usize,
    /// How deep children may nest. 1 = children, no grandchildren (0–3).
    pub max_depth: usize,
    /// Step limit for a child whose definition sets none (1–200).
    pub max_turns: usize,
    /// Children one task may start in total (1–64).
    pub max_per_task: usize,
}
impl Default for SubagentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_parallel: 4,
            max_depth: 1,
            max_turns: 24,
            max_per_task: 16,
        }
    }
}
impl SubagentsConfig {
    pub fn from_config(config: &Config) -> Self {
        let mut value: Self = config
            .extra
            .get("subagents")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        value.max_parallel = value.max_parallel.clamp(1, 8);
        value.max_depth = value.max_depth.min(3);
        value.max_turns = value.max_turns.clamp(1, 200);
        value.max_per_task = value.max_per_task.clamp(1, 64);
        value
    }
}

/// Tools a child may use. Deny wins; an empty allow list allows everything
/// its permissions allow. `mcp__server` in a list covers that server's tools.
#[derive(Clone, Debug, Default)]
pub struct ToolFilter {
    allow: Option<BTreeSet<String>>,
    deny: BTreeSet<String>,
}
impl ToolFilter {
    pub fn new(allow: &[String], deny: &[String]) -> Self {
        Self {
            allow: (!allow.is_empty()).then(|| allow.iter().cloned().collect()),
            deny: deny.iter().cloned().collect(),
        }
    }
    fn listed(set: &BTreeSet<String>, tool: &str) -> bool {
        set.contains(tool)
            || (tool.starts_with("mcp__")
                && set
                    .iter()
                    .any(|p| p.starts_with("mcp__") && tool.starts_with(&format!("{p}__"))))
    }
    pub fn permits(&self, tool: &str) -> bool {
        if Self::listed(&self.deny, tool) {
            return false;
        }
        self.allow
            .as_ref()
            .is_none_or(|allow| Self::listed(allow, tool))
    }
}

/// Where a child's approval prompts are shown, and the label its reasons
/// start with (`Subagent general`, `Implement role (Codex)`).
#[derive(Clone, Debug)]
pub struct ApprovalRoute {
    pub session_id: String,
    pub label: String,
}
impl ApprovalRoute {
    pub fn reason(&self, reason: &str) -> String {
        format!("{}: {reason}", self.label)
    }
}

/// Per-task additions to the native tool executor: subagents, skills,
/// nested project guidance, a child's tool filter and approval routing.
#[derive(Clone, Default)]
pub struct ToolExtensions {
    pub host: Option<Arc<SubagentHost>>,
    pub filter: Option<Arc<ToolFilter>>,
    pub approval: Option<ApprovalRoute>,
    /// `(name, description, path)` of skills the model may load.
    pub skills: Arc<Vec<(String, String, String)>>,
    pub guidance: Option<Arc<NestedGuidance>>,
    /// The user's profile merged with the project, for `load_skill`.
    pub rulebook: Option<Arc<crate::rulebook::Book>>,
}

fn schema(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"type":"function","function":{"name":name,"description":description,
        "parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}})
}

pub fn spawn_schema() -> Value {
    let s = json!({"type":"string"});
    let task = json!({
        "agent": {"type":"string","description":"Agent name, e.g. explore, plan, review, general"},
        "prompt": {"type":"string","description":"Complete, self-contained task. The subagent does not see this conversation."},
        "description": {"type":"string","description":"Short label (3-6 words)"},
        "model": s,
        "write": {"type":"boolean","description":"Edit files in an isolated worktree (write agents only); the diff comes back to you"},
    });
    let mut properties = task.clone();
    properties["tasks"] = json!({"type":"array","maxItems":MAX_BATCH,"description":"Several subagents to run in parallel","items":{"type":"object","properties":task,"required":["prompt"],"additionalProperties":false}});
    schema(
        "spawn_agent",
        "Start subagents with their own context for focused work (search, planning, review, isolated edits). Give prompt, or tasks to run several in parallel. Returns each result summary and any diff.",
        properties,
        &[],
    )
}

impl ToolExtensions {
    pub fn schemas(&self) -> Vec<Value> {
        let mut schemas = Vec::new();
        if self.host.is_some() {
            schemas.push(spawn_schema());
            schemas.push(schema(
                "apply_agent_changes",
                "Apply a write subagent's diff to this project by run_id. Checkpointed; edits ask for approval as usual.",
                json!({"run_id":{"type":"string"}}),
                &["run_id"],
            ));
        }
        if !self.skills.is_empty() {
            schemas.push(schema(
                "load_skill",
                "Load a project skill's instructions by name when its description fits the task.",
                json!({"name":{"type":"string"}}),
                &["name"],
            ));
        }
        schemas
    }
    pub fn permits(&self, tool: &str) -> bool {
        self.filter.as_ref().is_none_or(|f| f.permits(tool))
    }
    /// Lists agents and skills for the system prompt, only for offered tools.
    pub fn context_note(&self, schemas: &[Value]) -> String {
        let offered = |name: &str| schemas.iter().any(|s| s["function"]["name"] == name);
        let mut note = String::new();
        if let Some(host) = self.host.as_ref().filter(|_| offered("spawn_agent")) {
            note.push_str("\n\nSubagents (spawn_agent): delegate focused, self-contained work so your own context stays small; independent tasks can run in parallel via tasks. A subagent sees only the prompt you give it. Read-only agents cannot change files; write agents edit an isolated worktree and return a diff that you review and apply with apply_agent_changes. Available agents:");
            for agent in host.catalog().agents.iter().take(32) {
                // The project's roles say which model runs an agent.
                let runs_on = match host.role_model(agent) {
                    Some((role, id)) if agent.model.is_none() => {
                        format!("; {} role on {id}", role.id())
                    }
                    _ => String::new(),
                };
                note.push_str(&format!(
                    "\n- {} ({}{runs_on}): {}",
                    agent.name,
                    if agent.read_only() {
                        "read-only"
                    } else {
                        "write"
                    },
                    truncate(&agent.description, 200)
                ));
            }
        }
        if offered("load_skill") {
            note.push_str(&crate::rulebook::delivery::native_skill_index(&self.skills));
        }
        note
    }
    /// The session and reason an approval prompt is shown with.
    pub fn approval_target(&self, own_session: &str, reason: String) -> (String, String) {
        match &self.approval {
            Some(route) => (route.session_id.clone(), route.reason(&reason)),
            None => (own_session.to_owned(), reason),
        }
    }
    pub fn load_skill(&self, workspace: &Workspace, args: &Value) -> Result<Value> {
        let name = args["name"].as_str().context("Give the skill name")?;
        ensure!(
            self.skills.iter().any(|(n, _, _)| n == name),
            "No model-loadable skill named '{name}'"
        );
        match &self.rulebook {
            Some(book) => crate::workflows::load_from(&book.catalog(workspace), name, SKILL_BYTES),
            None => crate::workflows::load_skill(workspace, name, SKILL_BYTES),
        }
    }
}

/// The parent task a host starts children for.
pub struct ParentContext {
    pub job_id: String,
    pub session_id: String,
    pub task_id: String,
    pub workspace: PathBuf,
    pub config: Config,
    pub cancel: CancellationToken,
    pub events: TaskEvents,
    /// 0 for a top-level task.
    pub depth: usize,
    /// The local model this task or one of its ancestors holds while its
    /// children run. Only one local model runs at a time, so a child on
    /// another local model would wait for its parent: it is refused instead.
    pub holds_local: Option<String>,
}

pub struct SubagentHost {
    engine: Engine,
    parent: ParentContext,
    settings: SubagentsConfig,
    catalog: AgentCatalog,
    slots: Arc<Semaphore>,
    spawned: AtomicUsize,
    /// The project's roles (`crate::roles`).
    roles: roles::Setup,
    /// Cloud providers this conversation allowed as roles.
    consented: std::collections::BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunRecord {
    pub id: String,
    pub agent: String,
    pub description: String,
    pub prompt: String,
    /// read-only | write
    pub mode: String,
    pub model: String,
    pub parent_session: String,
    pub parent_task: String,
    pub parent_job: String,
    pub job_id: String,
    pub session_id: String,
    /// running | completed | failed | cancelled | …
    pub status: String,
    pub summary: String,
    pub error: Option<String>,
    pub files: Vec<compare::FileStat>,
    pub files_truncated: bool,
    pub binary_files: Vec<String>,
    /// A patch was saved for `apply_agent_changes`.
    pub patch: bool,
    pub applied: bool,
    pub usage: Value,
    pub steps: usize,
    pub depth: usize,
    pub notes: Vec<String>,
    pub created_at: f64,
    pub finished_at: Option<f64>,
    /// The role this run played (`plan`, `implement`, `review`, `explore`),
    /// empty when its model did not come from the project's roles.
    pub role: String,
    /// The picker id the run used.
    pub model_id: String,
    /// `shadowcode` (ShadowCode's own loop) or `vendor` (a vendor CLI).
    pub runner: String,
    /// The vendor CLI (`codex`, `claude`, …) when `runner` is `vendor`.
    pub vendor: Option<String>,
    /// `local` (this computer) or `cloud`.
    pub route: String,
    /// How the run is paid for: `local`, `subscription` or `api`.
    pub cost: String,
    /// A review role's `Verdict:` line: `ready` or `needs_changes`.
    pub verdict: Option<String>,
}

impl RunRecord {
    /// The fields `subagent.started` and `subagent.finished` share.
    fn event(&self) -> Value {
        json!({
            "run_id": self.id,
            "agent": self.agent,
            "description": self.description,
            "mode": self.mode,
            "model": self.model,
            "model_id": self.model_id,
            "role": self.role,
            "runner": self.runner,
            "vendor": self.vendor,
            "route": self.route,
            "cost": self.cost,
            "job_id": self.job_id,
            "session_id": self.session_id,
            "depth": self.depth,
        })
    }
}

use crate::store::keys::{subagent_index as index_key, subagent_run as record_key};
fn valid_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "Unknown subagent run"
    );
    Ok(())
}
static RECORDS: Mutex<()> = Mutex::new(());
pub fn get(store: &Store, id: &str) -> Result<RunRecord> {
    valid_id(id)?;
    let text = store
        .native_meta(&record_key(id))?
        .context("Subagent run not found")?;
    Ok(serde_json::from_str(&text)?)
}
fn save(store: &Store, record: &RunRecord) -> Result<()> {
    let _guard = RECORDS
        .lock()
        .map_err(|_| anyhow::anyhow!("Subagent record lock poisoned"))?;
    let key = index_key(&record.parent_session);
    let mut ids: Vec<String> = store
        .native_meta(&key)?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    if !ids.contains(&record.id) {
        ids.push(record.id.clone());
        let excess = ids.len().saturating_sub(INDEXED);
        ids.drain(..excess);
        store.set_native_meta(&key, &serde_json::to_string(&ids)?)?;
    }
    store.set_native_meta(&record_key(&record.id), &serde_json::to_string(record)?)
}
/// After a restart: runs still marked running were stopped with the app.
/// Each is marked interrupted and its parent conversation gets the
/// `subagent.finished` event it never received, so its card stops showing
/// progress. Returns how many were recovered.
pub fn recover(store: &Store) -> Result<usize> {
    let _guard = RECORDS
        .lock()
        .map_err(|_| anyhow::anyhow!("Subagent record lock poisoned"))?;
    let rows = store.query(
        "SELECT value FROM native_meta WHERE key LIKE 'subagent:%'",
        [],
    )?;
    let mut recovered = 0;
    for row in rows {
        let Some(mut record) = row["value"]
            .as_str()
            .and_then(|text| serde_json::from_str::<RunRecord>(text).ok())
        else {
            continue;
        };
        if record.status != "running" || valid_id(&record.id).is_err() {
            continue;
        }
        record.status = "interrupted".into();
        record.error = Some("ShadowCode stopped before this subagent finished.".into());
        record.finished_at = Some(crate::now());
        if record.mode == "write" {
            record
                .notes
                .push("Its worktree, if one was left, is listed in Tools › Worktrees.".into());
        }
        store.set_native_meta(&record_key(&record.id), &serde_json::to_string(&record)?)?;
        if store.session(&record.parent_session)?.is_some() {
            let mut payload = record.event();
            crate::config::merge(
                &mut payload,
                json!({
                    "status": record.status,
                    "summary": "",
                    "error": record.error,
                    "files": [],
                    "files_truncated": false,
                    "binary_files": [],
                    "patch": false,
                    "usage": record.usage,
                    "steps": record.steps,
                    "notes": record.notes,
                    "interrupted": true,
                }),
            );
            store.add_event(
                "subagent.finished",
                &payload,
                Some(&record.parent_session),
                (!record.parent_task.is_empty()).then_some(record.parent_task.as_str()),
            )?;
        }
        recovered += 1;
    }
    Ok(recovered)
}
/// Runs started from one parent conversation, oldest first.
pub fn list(store: &Store, parent_session: &str) -> Result<Vec<RunRecord>> {
    let ids: Vec<String> = store
        .native_meta(&index_key(parent_session))?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    Ok(ids.iter().filter_map(|id| get(store, id).ok()).collect())
}
fn patch_path(engine: &Engine, id: &str) -> Result<PathBuf> {
    valid_id(id)?;
    let dir = engine.paths().data.join("subagents");
    crate::paths::private_directory(&dir)?;
    Ok(dir.join(format!("{id}.patch")))
}
/// Remove the saved patches of deleted runs (their conversation was
/// deleted). A patch that is already gone is fine.
pub fn remove_patches(paths: &crate::paths::AppPaths, runs: &[String]) {
    let dir = paths.data.join("subagents");
    for id in runs.iter().filter(|id| valid_id(id).is_ok()) {
        let _ = std::fs::remove_file(dir.join(format!("{id}.patch")));
    }
}

struct TaskArgs {
    agent: String,
    prompt: String,
    description: String,
    model: Option<String>,
    write: Option<bool>,
}
fn task_args(value: &Value) -> Result<TaskArgs> {
    let object = value
        .as_object()
        .context("Each subagent task must be an object")?;
    for key in object.keys() {
        ensure!(
            matches!(
                key.as_str(),
                "agent" | "prompt" | "description" | "model" | "write"
            ),
            "Unknown spawn_agent argument '{key}'"
        );
    }
    let text = |key: &str| -> Result<Option<String>> {
        match &value[key] {
            Value::Null => Ok(None),
            Value::String(s) => Ok(Some(s.trim().to_owned()).filter(|s| !s.is_empty())),
            _ => bail!("spawn_agent {key} must be text"),
        }
    };
    let prompt = text("prompt")?.context("Give the subagent a prompt")?;
    ensure!(
        prompt.len() <= 64_000,
        "Subagent prompt exceeds 64000 bytes"
    );
    let write = match &value["write"] {
        Value::Null => None,
        Value::Bool(b) => Some(*b),
        _ => bail!("spawn_agent write must be true or false"),
    };
    Ok(TaskArgs {
        agent: text("agent")?.unwrap_or_else(|| "general".into()),
        description: text("description")?
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect(),
        model: text("model")?,
        prompt,
        write,
    })
}

/// One child run, fully decided: what it runs, where and on which model.
pub(crate) struct ChildRun {
    pub definition: AgentDefinition,
    pub description: String,
    /// The task as the child receives it.
    pub prompt: String,
    pub model: ModelConfig,
    pub write: bool,
    pub notes: Vec<String>,
    /// Set when the project's roles chose the model.
    pub role: Option<Role>,
    /// Step limit for ShadowCode's own loop.
    pub max_turns: usize,
    /// Replaces the standard subagent introduction in the system prompt.
    pub system_context: Option<String>,
    /// Approval reasons start with this (default `Subagent <name>`).
    pub approval_label: Option<String>,
}

/// A finished child: its record and the text diff of a write child.
pub(crate) struct ChildOutcome {
    pub record: RunRecord,
    pub diff: String,
}

impl SubagentHost {
    pub fn new(engine: Engine, parent: ParentContext, settings: SubagentsConfig) -> Result<Self> {
        let workspace = Workspace::open(&parent.workspace)?;
        let catalog = agents::discover_for(engine.paths(), &workspace);
        let store = engine.store();
        let project = roles::project_of(&store, &workspace.path, Some(&parent.session_id));
        let roles = roles::load(&store, &project).unwrap_or_default();
        let consented = roles::consented(&store, &parent.session_id);
        Ok(Self {
            slots: Arc::new(Semaphore::new(settings.max_parallel)),
            engine,
            parent,
            settings,
            catalog,
            spawned: AtomicUsize::new(0),
            roles,
            consented,
        })
    }
    pub fn catalog(&self) -> &AgentCatalog {
        &self.catalog
    }
    /// The model a definition runs on by default: its role's, when the
    /// project names one.
    pub fn role_model(&self, definition: &AgentDefinition) -> Option<(Role, String)> {
        let role = Role::for_agent(definition)?;
        let setting = self.roles.get(role).trim();
        (!matches!(setting, "" | roles::SKIP)).then(|| (role, setting.to_owned()))
    }
    /// The `spawn_agent` tool. One task returns its result object; `tasks`
    /// returns `{results:[…]}`. Failures of a child are reported in its
    /// result, never as a failure of the parent task.
    pub async fn spawn(&self, args: &Value) -> Result<Value> {
        ensure!(args.is_object(), "spawn_agent arguments must be an object");
        let batch = match args.get("tasks") {
            Some(Value::Array(items)) => {
                ensure!(
                    args.as_object().is_some_and(|o| o.len() == 1),
                    "Give either tasks or a single prompt, not both"
                );
                ensure!(
                    (1..=MAX_BATCH).contains(&items.len()),
                    "Give between 1 and {MAX_BATCH} tasks"
                );
                Some(items.iter().map(task_args).collect::<Result<Vec<_>>>()?)
            }
            Some(_) => bail!("spawn_agent tasks must be a list"),
            None => None,
        };
        match batch {
            None => Ok(self.run_one(task_args(args)?).await),
            Some(tasks) => {
                let results =
                    futures_util::future::join_all(tasks.into_iter().map(|t| self.run_one(t)))
                        .await;
                let failed = results.iter().filter(|r| r["ok"] != true).count();
                let mut value = json!({"ok": failed == 0, "results": results});
                if failed > 0 {
                    value["error"] = json!(format!(
                        "{failed} of {} subagents did not complete",
                        value["results"].as_array().map_or(0, Vec::len)
                    ));
                }
                Ok(value)
            }
        }
    }

    fn resolve_model(
        &self,
        requested: Option<&str>,
        definition: &AgentDefinition,
        notes: &mut Vec<String>,
    ) -> Result<(ModelConfig, Option<Role>)> {
        let store = self.engine.store();
        let parent = &self.parent.config.model;
        let mut role = None;
        let model = if let Some(id) = requested {
            crate::model_registry::resolve(&store, id, parent)
                .with_context(|| format!("Could not use model {id}"))?
        } else if let Some(id) = &definition.model {
            match crate::model_registry::resolve(&store, id, parent) {
                Ok(model) => model,
                Err(_) => {
                    notes.push(format!(
                        "Agent {} asks for model '{id}', which is not a ShadowCode model id; it used this task's model.",
                        definition.name
                    ));
                    parent.clone()
                }
            }
        } else if let Some((for_role, id)) = self.role_model(definition) {
            role = Some(for_role);
            crate::model_registry::resolve(&store, &id, parent).with_context(|| {
                format!(
                    "The {} role's model ({id}) is not available. Choose another one in Settings › Roles.",
                    for_role.id()
                )
            })?
        } else {
            parent.clone()
        };
        ensure!(
            model.provider != "mock",
            "Choose a local or compatible model for subagents"
        );
        let what = match role {
            Some(role) => format!("the {} role", role.id()),
            None => "a subagent".into(),
        };
        let name = roles::model_label(&model);
        ensure!(
            !self.parent.config.offline() || crate::config::runs_on_this_computer(&model),
            "Offline mode: {what} cannot use {name}, which runs in the cloud. Choose a model that runs on this computer."
        );
        if let Some(vendor) = Vendor::from_provider(&model.provider) {
            ensure!(
                self.parent.config.cli_agents.vendor_enabled(vendor),
                "{} is disabled in Settings → Advanced, so {what} cannot use it",
                vendor.product_label()
            );
        }
        // Consent before cloud: a subagent has no interactive consent channel,
        // so when this conversation runs on this computer a subagent may not
        // move it to a cloud route unless the user allowed that provider for
        // this conversation (a Plan → Implement → Review turn or an @agent
        // request asked first). This blocks a repository agent file or an
        // injected spawn_agent argument from silently sending project files
        // to a cloud provider (and billing the user's key or plan).
        ensure!(
            subagent_cloud_allowed(parent, &model, &self.consented),
            "This conversation runs on this computer; {what} cannot use the cloud model '{}' ({name}) without your consent. Start the request with @{} so ShadowCode can ask you first, switch this conversation to that model, or choose a model on this computer.",
            model.provider,
            definition.name
        );
        if let Some(conflict) = local_conflict(self.parent.holds_local.as_deref(), &model, &what) {
            bail!(conflict);
        }
        Ok((model, role))
    }

    async fn run_one(&self, task: TaskArgs) -> Value {
        let label = task.agent.clone();
        match self.try_run(task).await {
            Ok(value) => value,
            Err(error) => {
                json!({"ok": false, "agent": label, "status": "failed", "error": format!("{error:#}")})
            }
        }
    }

    async fn try_run(&self, task: TaskArgs) -> Result<Value> {
        ensure!(!self.parent.cancel.is_cancelled(), "The task was cancelled");
        let definition = self.catalog.get(&task.agent).cloned().with_context(|| {
            format!(
                "Unknown agent '{}'. Available: {}",
                task.agent,
                self.catalog
                    .agents
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        ensure!(
            self.spawned.fetch_add(1, Ordering::AcqRel) < self.settings.max_per_task,
            "This task already started {} subagents (subagents.max_per_task)",
            self.settings.max_per_task
        );
        let write = task.write.unwrap_or(!definition.read_only());
        ensure!(
            !write || !definition.read_only(),
            "Agent {} is read-only; use a write agent such as general for edits",
            definition.name
        );
        ensure!(
            !write || self.parent.config.permissions.level != PermissionLevel::ReadOnly,
            "This task is read-only, so subagents cannot edit files; use a read-only agent"
        );
        let mut notes = Vec::new();
        let (model, role) = self.resolve_model(task.model.as_deref(), &definition, &mut notes)?;
        let max_turns = definition.max_turns.unwrap_or(self.settings.max_turns);
        let outcome = self
            .execute(ChildRun {
                description: task.description.clone(),
                prompt: task.prompt.clone(),
                model,
                write,
                notes,
                role,
                max_turns,
                system_context: None,
                approval_label: None,
                definition,
            })
            .await?;
        let ChildOutcome { record, diff } = outcome;
        let mut result = json!({
            "ok": record.status == "completed",
            "run_id": record.id,
            "agent": record.agent,
            "mode": record.mode,
            "model": record.model,
            "status": record.status,
            "summary": truncate(&record.summary, SUMMARY_FOR_MODEL),
            "session_id": record.session_id,
        });
        if !record.role.is_empty() {
            result["role"] = json!(record.role);
        }
        if let Some(error) = &record.error {
            result["error"] = json!(truncate(error, 2000));
        }
        if !record.notes.is_empty() {
            result["notes"] = json!(record.notes);
        }
        if write {
            result["files"] = json!(record
                .files
                .iter()
                .map(|f| format!(
                    "{} {} (+{} -{})",
                    f.status, f.path, f.additions, f.deletions
                ))
                .collect::<Vec<_>>());
            if record.patch {
                let shown = truncate(&diff, DIFF_FOR_MODEL);
                result["diff"] = json!(shown);
                result["diff_truncated"] = json!(shown.len() < diff.len());
                result["apply"] = json!(format!(
                    "Review the diff, then call apply_agent_changes with run_id {} to apply it to this project.",
                    record.id
                ));
            } else {
                result["diff"] = json!("");
                result["apply"] = json!("The subagent made no text changes.");
            }
            if !record.binary_files.is_empty() {
                result["binary_files_not_included"] = json!(record.binary_files);
            }
        }
        Ok(result)
    }

    /// One stage of a Plan → Implement → Review task (`engine::roles`): the
    /// role's target was resolved and allowed when the task started; it is
    /// checked again here. `prompt` already carries the role's instructions,
    /// the request and the previous roles' results.
    pub(crate) async fn run_role(
        &self,
        target: &roles::Target,
        prompt: String,
        write: bool,
        description: &str,
    ) -> Result<ChildOutcome> {
        ensure!(!self.parent.cancel.is_cancelled(), "The task was cancelled");
        let guard = roles::Guard {
            config: &self.parent.config,
            conversation_local: handoff::is_local(&self.parent.config.model),
            consented: &self.consented,
        };
        match guard.check(target) {
            Ok(()) => {}
            Err(roles::Refusal::Blocked(reason)) => bail!(reason),
            Err(roles::Refusal::NeedsConsent) => bail!(
                "The {} role runs on {} in the cloud, and this conversation has not allowed that.",
                target.role.id(),
                target.name()
            ),
        }
        ensure!(
            !write || self.parent.config.permissions.level != PermissionLevel::ReadOnly,
            "This project is read-only, so the implement role cannot change files"
        );
        self.spawned.fetch_add(1, Ordering::AcqRel);
        let builtin = match target.role {
            Role::Plan => "plan",
            Role::Implement => "general",
            Role::Review => "review",
            Role::Explore => "explore",
        };
        let mut definition = agents::builtins()
            .into_iter()
            .find(|a| a.name == builtin)
            .context("Built-in agent missing")?;
        definition.instructions = roles::instructions(target.role).into();
        let max_turns = if target.role == Role::Implement {
            self.parent.config.agent.max_steps
        } else {
            definition
                .max_turns
                .unwrap_or(self.settings.max_turns)
                .max(32)
        };
        let system_context = format!(
            "You run as the {} role of a Plan → Implement → Review task, started by ShadowCode for one step. You cannot see the conversation; everything you need is in the task. Your final message is handed to the next role, so end with a concise, self-contained result. {}",
            target.role.id(),
            if write {
                "You work in an isolated copy of the project; your file changes come back as a diff for review."
            } else {
                "You are read-only: do not attempt to change files."
            }
        );
        self.execute(ChildRun {
            definition,
            description: description.into(),
            prompt,
            model: target.model.clone(),
            write,
            notes: Vec::new(),
            role: Some(target.role),
            max_turns,
            system_context: Some(system_context),
            approval_label: Some(format!("{} role ({})", target.role.label(), target.name())),
        })
        .await
    }

    /// Start one child, wait for it, collect a write child's diff, and
    /// report it in the parent conversation.
    pub(crate) async fn execute(&self, run: ChildRun) -> Result<ChildOutcome> {
        let ChildRun {
            definition,
            description,
            prompt,
            model,
            write,
            notes,
            role,
            max_turns,
            system_context,
            approval_label,
        } = run;
        let vendor = Vendor::from_provider(&model.provider);
        let mut config = self.parent.config.clone();
        config.model = model;
        config.agent.max_steps = max_turns.min(self.parent.config.agent.max_steps).max(1);
        if !write {
            config.permissions.level = PermissionLevel::ReadOnly;
        }
        let _slot = tokio::select! {
            slot = self.slots.acquire() => slot.context("Subagent scheduler stopped")?,
            _ = self.parent.cancel.cancelled() => bail!("The task was cancelled"),
        };
        let cancel = self.parent.cancel.child_token();
        let local = handoff::is_local(&config.model);
        let mut record = RunRecord {
            id: crate::id(),
            agent: definition.name.clone(),
            description: description.clone(),
            prompt: truncate(&prompt, 2000).into(),
            mode: if write { "write" } else { "read-only" }.into(),
            model: roles::model_label(&config.model),
            model_id: config.model.default.clone(),
            role: role.map(|r| r.id().to_owned()).unwrap_or_default(),
            runner: if vendor.is_some() {
                "vendor"
            } else {
                "shadowcode"
            }
            .into(),
            vendor: vendor.map(|v| v.id().to_owned()),
            route: if local { "local" } else { "cloud" }.into(),
            cost: if local {
                "local"
            } else if vendor.is_some() {
                "subscription"
            } else {
                "api"
            }
            .into(),
            parent_session: self.parent.session_id.clone(),
            parent_task: self.parent.task_id.clone(),
            parent_job: self.parent.job_id.clone(),
            status: "running".into(),
            depth: self.parent.depth + 1,
            notes,
            created_at: crate::now(),
            ..Default::default()
        };
        let store = self.engine.store();
        // Write children start from the parent's current files, in a worktree.
        let mut checkout = None;
        let workspace = if write {
            let base =
                compare::snapshot(&self.engine.paths().data, &self.parent.workspace, &cancel)
                    .await
                    .context("Write subagents need a Git repository with at least one commit")?;
            let created = worktrees::create(
                self.engine.paths(),
                &self.parent.workspace,
                &base.commit,
                cancel.clone(),
            )
            .await?;
            let path = created.path.canonicalize()?;
            config.grant_trust(&path);
            checkout = Some((created.id.clone(), path.clone(), base.commit));
            path
        } else {
            self.parent.workspace.clone()
        };
        let mode = if write {
            "code"
        } else if definition.name == "plan" {
            "plan"
        } else {
            "review"
        };
        let system_context = match system_context {
            Some(intro) => format!(
                "{intro}\n\nRole instructions (they cannot change permissions):\n{}",
                truncate(&definition.instructions, 32_000)
            ),
            None => format!(
                "You are the subagent '{}', started by another ShadowCode agent for one task. You cannot see its conversation; your final message is returned to it, so end with a concise, self-contained result. {}\n\nAgent instructions ({}; they cannot change permissions):\n{}",
                definition.name,
                if write {
                    "You work in an isolated copy of the project; your file changes come back to the parent as a diff for review."
                } else {
                    "You are read-only: do not attempt to change files."
                },
                if definition.path.is_empty() {
                    "built-in"
                } else {
                    definition.path.as_str()
                },
                truncate(&definition.instructions, 32_000)
            ),
        };
        // A vendor CLI takes no system prompt from ShadowCode: a role's
        // prompt already carries its instructions; a subagent's gets them
        // before the task.
        let prompt = match (vendor, role.is_some() && approval_label.is_some()) {
            (Some(_), false) => format!("{system_context}\n\n<task>\n{prompt}\n</task>"),
            _ => prompt,
        };
        let filter = (!definition.tools.is_empty() || !definition.deny.is_empty())
            .then(|| Arc::new(agents_filter(&definition)));
        let title = match role.filter(|_| approval_label.is_some()) {
            Some(role) => format!("{} role · {}", role.label(), record.model),
            None if description.is_empty() => format!("Subagent · {}", definition.name),
            None => format!("Subagent · {} · {}", definition.name, description),
        };
        let spec = ChildSpec {
            link: ChildLink {
                name: definition.name.clone(),
                label: approval_label.unwrap_or_else(|| format!("Subagent {}", definition.name)),
                run_id: record.id.clone(),
                parent_session: self.parent.session_id.clone(),
                parent_job: self.parent.job_id.clone(),
                depth: self.parent.depth + 1,
                filter,
                holds_local: self.parent.holds_local.clone(),
            },
            workspace,
            prompt,
            mode: mode.into(),
            config,
            system_context,
            cancel: cancel.clone(),
            web: self.parent.config.permissions.web,
            title,
        };
        let events = self.parent.events.clone();
        let started_record = Arc::new(Mutex::new(record.clone()));
        let hook_record = started_record.clone();
        let hook_store = store.clone();
        let started = move |job: &Job| {
            if let Ok(mut record) = hook_record.lock() {
                record.job_id = job.id.clone();
                record.session_id = job.session_id.clone();
                let _ = save(&hook_store, &record);
                let mut payload = record.event();
                payload["prompt"] = json!(truncate(&record.prompt, 400));
                let _ = events.emit("subagent.started", payload);
            }
        };
        let outcome = self.engine.run_child(spec, started).await;
        record = started_record.lock().map(|r| r.clone()).unwrap_or(record);
        match &outcome {
            Ok(job) => {
                record.status = job.status.clone();
                record.summary = job.summary.clone();
                record.steps = job.steps;
                record.usage = json!({
                    "prompt_tokens": job.usage.prompt_tokens,
                    "completion_tokens": job.usage.completion_tokens,
                    "total_tokens": job.usage.total_tokens,
                    "cost_usd": job.usage.cost_usd,
                    "cost_estimated": job.usage.cost_estimated,
                    "estimated": job.usage_is_estimated,
                    "source": job.usage.source,
                });
                if job.status != "completed" {
                    record.error = Some(job.summary.clone());
                }
                // The parent task's usage covers its subagents, whatever
                // their outcome: a failed child still used tokens.
                if let Err(error) = self.engine.add_child_usage(
                    &self.parent.job_id,
                    &self.parent.events,
                    &job.usage,
                    job.usage_is_estimated,
                ) {
                    record.notes.push(format!(
                        "Could not add this run's usage to the task: {error:#}"
                    ));
                }
            }
            Err(error) => {
                record.status = if cancel.is_cancelled() {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                record.error = Some(format!("{error:#}"));
            }
        }
        if record.role == Role::Review.id() && record.status == "completed" {
            record.verdict = roles::verdict(&record.summary).map(str::to_owned);
        }
        let mut diff = String::new();
        if let Some((id, path, base)) = checkout {
            // Collect whatever the child changed, even after a failure, then
            // always remove the worktree and its branch.
            let collect = CancellationToken::new();
            let collected = match self.collect(&record.id, &path, &base, &collect).await {
                Ok((files, truncated, binary, text)) => {
                    record.files = files;
                    record.files_truncated = truncated;
                    record.binary_files = binary;
                    record.patch = !text.is_empty();
                    diff = text;
                    true
                }
                Err(error) => {
                    // Its changes exist only in the worktree: keep it.
                    record.notes.push(format!(
                        "Could not read the subagent's changes: {error:#}. Its worktree was kept at {} so nothing is lost; review or remove it in Tools › Worktrees.",
                        path.display()
                    ));
                    false
                }
            };
            if collected {
                if let Err(error) =
                    worktrees::dispose(self.engine.paths(), &self.parent.workspace, &id, collect)
                        .await
                {
                    record.notes.push(format!(
                        "The worktree {} was kept: {error:#}",
                        path.display()
                    ));
                }
            }
        }
        record.finished_at = Some(crate::now());
        save(&store, &record)?;
        let mut payload = record.event();
        let finished = json!({
            "status": record.status,
            "summary": truncate(&record.summary, 4000),
            "error": record.error.as_deref().map(|e| truncate(e, 2000)),
            "files": record.files,
            "files_truncated": record.files_truncated,
            "binary_files": record.binary_files,
            "patch": record.patch,
            "usage": record.usage,
            "steps": record.steps,
            "notes": record.notes,
            "verdict": record.verdict,
            "duration_s": ((record.finished_at.unwrap_or(record.created_at) - record.created_at) * 10.0).round() / 10.0,
        });
        crate::config::merge(&mut payload, finished);
        self.parent.events.emit("subagent.finished", payload)?;
        Ok(ChildOutcome { record, diff })
    }

    /// Diffstat and a text patch of the worktree against its base; the
    /// patch is saved for `apply_agent_changes`.
    async fn collect(
        &self,
        run_id: &str,
        worktree: &std::path::Path,
        base: &str,
        cancel: &CancellationToken,
    ) -> Result<(Vec<compare::FileStat>, bool, Vec<String>, String)> {
        let (files, truncated) = compare::diffstat(worktree, base, cancel).await?;
        // The child's worktree is throwaway; `add` would otherwise run Git
        // inside any nested repository it planted (`crate::git_guard`).
        crate::git_guard::freeze_gitlinks(worktree, None).await?;
        compare::git(worktree, &["add", "--all"], cancel).await?;
        let patch = compare::git(
            worktree,
            &[
                "diff",
                "--cached",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                base,
                "--",
            ],
            cancel,
        )
        .await?;
        let binary: Vec<String> = files
            .iter()
            .filter(|f| f.binary)
            .map(|f| f.path.clone())
            .collect();
        // Binary hunks cannot go through the text patch tool.
        let mut text = String::new();
        let mut keep = true;
        for line in patch.lines() {
            if line.starts_with("diff --git ") {
                keep = !binary.iter().any(|b| line.ends_with(&format!(" b/{b}")));
            }
            if keep {
                text.push_str(line);
                text.push('\n');
            }
        }
        ensure!(
            text.len() <= 8_000_000,
            "The subagent's diff exceeds 8 MB; inspect its session instead"
        );
        if !text.trim().is_empty() {
            crate::paths::atomic_write(&patch_path(&self.engine, run_id)?, text.as_bytes(), true)?;
        } else {
            text.clear();
        }
        Ok((files, truncated, binary, text))
    }

    /// The saved patch of a write run started from this conversation.
    pub fn patch_for(&self, args: &Value) -> Result<(String, String)> {
        let id = args["run_id"].as_str().context("Give the run_id")?.trim();
        let store = self.engine.store();
        let record = get(&store, id)?;
        ensure!(
            record.parent_session == self.parent.session_id,
            "That subagent run belongs to another conversation"
        );
        ensure!(record.mode == "write", "That subagent was read-only");
        ensure!(!record.applied, "Those changes were already applied");
        ensure!(record.patch, "That subagent left no text changes to apply");
        let text = std::fs::read_to_string(patch_path(&self.engine, id)?)
            .context("The subagent's saved diff is missing")?;
        Ok((record.id, text))
    }
    pub fn mark_applied(&self, run_id: &str, output: &Value) {
        let store = self.engine.store();
        if let Ok(mut record) = get(&store, run_id) {
            record.applied = true;
            let _ = save(&store, &record);
            let _ = self.parent.events.emit(
                "subagent.applied",
                json!({"run_id": run_id, "agent": record.agent, "role": record.role, "paths": output["paths"]}),
            );
        }
    }
}

fn agents_filter(definition: &AgentDefinition) -> ToolFilter {
    let mut allow = definition.tools.clone();
    if !allow.is_empty() {
        // Plans stay visible whatever the list says.
        allow.push("update_plan".into());
    }
    ToolFilter::new(&allow, &definition.deny)
}

/// One local model at a time: a child on another local model than the one
/// its parent (or an ancestor) holds would wait for that parent to finish,
/// which waits for the child. Refused with a clear message instead.
fn local_conflict(held: Option<&str>, child: &ModelConfig, what: &str) -> Option<String> {
    let held = held?;
    if !crate::local_engine::is_managed(child) || child.default == held {
        return None;
    }
    let held_name = crate::local_engine::known(held)
        .map(|e| e.name)
        .unwrap_or_else(|| held.to_owned());
    Some(format!(
        "Only one model on this computer runs at a time. This conversation is using {held_name}, so {what} cannot run {} now. Choose {held_name} or a cloud model for it, or turn on Plan → Implement → Review, where roles run one after another.",
        roles::model_label(child)
    ))
}

/// Consent before cloud: a subagent may move to a cloud route only when the
/// conversation is already on one, or the user allowed that provider for this
/// conversation. It has no interactive consent channel, so a local
/// conversation never silently spawns a cloud child (from a repository agent
/// file, a project role, or an injected `spawn_agent` model argument).
fn subagent_cloud_allowed(
    parent: &ModelConfig,
    child: &ModelConfig,
    consented: &std::collections::BTreeSet<String>,
) -> bool {
    handoff::is_local(child) || !handoff::is_local(parent) || consented.contains(&child.provider)
}

#[cfg(test)]
mod cloud_consent_tests {
    use super::{local_conflict, subagent_cloud_allowed};
    use crate::config::ModelConfig;
    use std::collections::BTreeSet;

    fn model(provider: &str, default: &str, endpoint: &str) -> ModelConfig {
        ModelConfig {
            provider: provider.into(),
            default: default.into(),
            endpoint: endpoint.into(),
            ..ModelConfig::default()
        }
    }

    #[test]
    fn a_local_conversation_never_spawns_a_cloud_subagent() {
        let local = model("llamacpp", "local:gguf:x", "http://127.0.0.1:8080/v1");
        let cloud = model(
            "openrouter",
            "api:openrouter:a/b",
            "https://openrouter.ai/api/v1",
        );
        let none = BTreeSet::new();
        // Local parent: a cloud child is refused, a local child is fine.
        assert!(!subagent_cloud_allowed(&local, &cloud, &none));
        assert!(subagent_cloud_allowed(&local, &local, &none));
        // Cloud parent (the conversation already consented): either is fine.
        assert!(subagent_cloud_allowed(&cloud, &cloud, &none));
        assert!(subagent_cloud_allowed(&cloud, &local, &none));
        // The user allowed this provider for this conversation.
        let allowed: BTreeSet<String> = ["openrouter".to_owned()].into();
        assert!(subagent_cloud_allowed(&local, &cloud, &allowed));
        let claude = crate::cli_agent::resolve_vendor("cli:claude").unwrap();
        assert!(!subagent_cloud_allowed(&local, &claude, &allowed));
    }

    #[test]
    fn a_child_never_waits_for_the_local_model_its_parent_holds() {
        let a = model("llamacpp", "local:gguf:aaa", "");
        let b = model("llamacpp", "local:gguf:bbb", "");
        let cloud = crate::cli_agent::resolve_vendor("cli:codex").unwrap();
        // Nothing held (a cloud parent or a Plan → Implement → Review task):
        // any local model waits for the runtime as usual.
        assert!(local_conflict(None, &b, "the review role").is_none());
        // The parent holds A: A is shared, a cloud child is fine, B is refused.
        assert!(local_conflict(Some("local:gguf:aaa"), &a, "x").is_none());
        assert!(local_conflict(Some("local:gguf:aaa"), &cloud, "x").is_none());
        let refused = local_conflict(Some("local:gguf:aaa"), &b, "the review role").unwrap();
        assert!(
            refused.contains("Only one model on this computer runs at a time"),
            "{refused}"
        );
        assert!(refused.contains("the review role cannot run"), "{refused}");
        assert!(refused.contains("Plan → Implement → Review"), "{refused}");
    }
}

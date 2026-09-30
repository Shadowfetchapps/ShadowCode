//! Roles: which model plans, implements, reviews and explores in a project.
//!
//! A project's role setup names a model or a vendor CLI runner per role. It
//! is stored by ShadowCode (`native_meta`), never read from repository files,
//! so a repository cannot redirect model traffic. Two things use it:
//!
//! - **Subagents.** `explore`, `plan` and `review` run on their role's model,
//!   and write subagents (`general`, or a definition with `mode: write`) on the
//!   implement role's (`crate::subagents`). An empty role keeps today's
//!   behaviour: the subagent runs on the conversation's model.
//! - **Plan → Implement → Review.** With `pipeline` on, a Code task runs its
//!   roles in turn (`engine::roles`): the plan role writes a plan, the
//!   implement role changes an isolated worktree, the review role reviews that
//!   diff, and the conversation applies it with the usual edit approval. A
//!   Plan task runs the plan role only.
//!
//! A role on a vendor CLI (Codex, Claude Code, Cursor, Grok, Antigravity) is a
//! normal vendor job in the role's own child conversation.
//!
//! Guardrails:
//! - Offline, a role that runs in the cloud is refused.
//! - A conversation that runs on this computer sends nothing to a cloud role
//!   without the user's consent for that provider ([`CONSENT_META`]); the
//!   window asks with the usual consent dialog before the turn starts.
//! - Only one local GGUF model runs at a time. Roles of a Plan → Implement →
//!   Review task run one after another, so each may use its own local model;
//!   a subagent started while the conversation's own local model is loaded
//!   must use that model or a cloud one (`crate::subagents`).
use crate::{
    agents::AgentDefinition,
    cli_agent::{handoff, Vendor},
    config::{Config, ModelConfig},
    store::Store,
    tools::truncate,
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// A role's setting that skips that step of Plan → Implement → Review (plan
/// and review only). For subagents it means the conversation's model.
pub const SKIP: &str = "skip";
/// Preset placeholder for "a model on this computer", replaced by a concrete
/// `local:gguf:` id when the preset is applied.
pub const LOCAL: &str = "local";
/// `session_meta`: the cloud providers (`cli:claude`, `openrouter`, …) the
/// user agreed may receive this conversation's work as a role (JSON list).
/// Only an answered consent dialog adds to it.
pub const CONSENT_META: &str = "consent:cloud_roles";
/// `session_meta`: cloud providers that received this conversation as a role
/// while it ran in the cloud, so no consent was needed (JSON list). They are
/// not asked again about those earlier cloud turns; once a turn ran on this
/// computer, they are asked like any other provider.
pub const SEEN_META: &str = "roles:cloud_seen";
/// The routing provider recorded on a Plan → Implement → Review task, so the
/// next turn on any single model receives it as a handoff (`handoff::build`).
pub const PROVIDER: &str = "shadowcode:roles";
/// Plan text handed to the implement and review roles.
const PLAN_FOR_ROLE: usize = 8_000;
/// Diff text handed to the review role (the full patch stays on disk).
const DIFF_FOR_REVIEW: usize = 24_000;
const REQUEST_FOR_ROLE: usize = 16_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Plan,
    Implement,
    Review,
    Explore,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Plan, Role::Implement, Role::Review, Role::Explore];
    pub fn id(self) -> &'static str {
        match self {
            Role::Plan => "plan",
            Role::Implement => "implement",
            Role::Review => "review",
            Role::Explore => "explore",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Role::Plan => "Plan",
            Role::Implement => "Implement",
            Role::Review => "Review",
            Role::Explore => "Explore",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.id() == value.trim())
    }
    /// Plan and review may be left out of Plan → Implement → Review.
    pub fn skippable(self) -> bool {
        matches!(self, Role::Plan | Role::Review)
    }
    /// The role a subagent definition runs as: the built-in names, and any
    /// write agent as the implement role. Other read-only agents have none.
    pub fn for_agent(definition: &AgentDefinition) -> Option<Self> {
        match definition.name.as_str() {
            "explore" => Some(Role::Explore),
            "plan" => Some(Role::Plan),
            "review" => Some(Role::Review),
            _ if !definition.read_only() => Some(Role::Implement),
            _ => None,
        }
    }
}

/// A project's roles. Each value is `""` (the conversation's model), `skip`
/// (plan and review), or a picker id (`cli:claude`, `cli:codex:gpt-…`,
/// `local:gguf:…`, `api:openrouter:…`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Setup {
    /// Code tasks run as Plan → Implement → Review.
    pub pipeline: bool,
    pub plan: String,
    pub implement: String,
    pub review: String,
    pub explore: String,
    /// The preset last applied; cleared when a role is changed by hand.
    pub preset: String,
    pub updated_at: f64,
}

impl Setup {
    pub fn get(&self, role: Role) -> &str {
        match role {
            Role::Plan => &self.plan,
            Role::Implement => &self.implement,
            Role::Review => &self.review,
            Role::Explore => &self.explore,
        }
    }
    pub fn set(&mut self, role: Role, value: &str) {
        *self.slot(role) = value.to_owned();
    }
    fn slot(&mut self, role: Role) -> &mut String {
        match role {
            Role::Plan => &mut self.plan,
            Role::Implement => &mut self.implement,
            Role::Review => &mut self.review,
            Role::Explore => &mut self.explore,
        }
    }
    pub fn validate(&self) -> Result<()> {
        for role in Role::ALL {
            let value = self.get(role);
            ensure!(
                value.len() <= 1024 && !value.chars().any(char::is_control),
                "The {} role's model id is not valid",
                role.id()
            );
            ensure!(
                value != SKIP || role.skippable(),
                "The {} role cannot be skipped",
                role.id()
            );
            ensure!(
                value != LOCAL,
                "Choose a model on this computer for the {} role",
                role.id()
            );
        }
        ensure!(
            self.preset.len() <= 64,
            "Preset ids are at most 64 characters"
        );
        Ok(())
    }
    /// Any role names a model (not the conversation's own).
    pub fn any(&self) -> bool {
        Role::ALL
            .iter()
            .any(|role| !matches!(self.get(*role), "" | SKIP))
    }
}

/// A named role setup.
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// Plan, implement, review, explore.
    pub roles: [&'static str; 4],
}

pub const PRESETS: [Preset; 4] = [
    Preset {
        id: "claude-codex-local",
        label: "Claude Code plans, Codex implements, local reviews",
        description: "Claude Code writes the plan, Codex makes the changes and a model on this computer reviews them.",
        roles: ["cli:claude", "cli:codex", LOCAL, ""],
    },
    Preset {
        id: "claude-plans",
        label: "Claude Code plans",
        description: "Claude Code writes the plan; the conversation's model makes the changes. No review step.",
        roles: ["cli:claude", "", SKIP, ""],
    },
    Preset {
        id: "local-review",
        label: "Local model reviews",
        description: "The conversation's model makes the changes and a model on this computer reviews them. No plan step.",
        roles: [SKIP, "", LOCAL, ""],
    },
    Preset {
        id: "all-local",
        label: "Everything on this computer",
        description: "One model on this computer plans, implements, reviews and explores. Nothing leaves this computer.",
        roles: [LOCAL, LOCAL, LOCAL, LOCAL],
    },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Apply a preset. `local` is the local model its `local` roles use; a
/// preset that needs one fails without it.
pub fn apply_preset(setup: &mut Setup, preset: &Preset, local: Option<&str>) -> Result<()> {
    for (role, value) in Role::ALL.into_iter().zip(preset.roles) {
        let value = if value == LOCAL {
            local
                .context("No model on this computer is ready. Add one in Settings › Local models, then choose this preset again.")?
                .to_owned()
        } else {
            value.to_owned()
        };
        *setup.slot(role) = value;
    }
    setup.preset = preset.id.into();
    Ok(())
}

/// `native_meta` key of a project's role setup.
pub fn key(workspace: &Path) -> String {
    format!("roles:{}", workspace.display())
}

pub fn load(store: &Store, workspace: &Path) -> Result<Setup> {
    Ok(store
        .native_meta(&key(workspace))?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default())
}

pub fn save(store: &Store, workspace: &Path, setup: &Setup) -> Result<()> {
    setup.validate()?;
    let mut setup = setup.clone();
    setup.updated_at = crate::now();
    store.set_native_meta(&key(workspace), &serde_json::to_string(&setup)?)
}

/// The project a conversation belongs to (its roles, its "Always allow"
/// rules): a worktree task's own project, else the workspace the
/// conversation runs in. A subagent's or role's conversation, which may run
/// in a throwaway worktree, belongs to its parent conversation's project.
pub fn project_of(store: &Store, workspace: &Path, session: Option<&str>) -> PathBuf {
    use crate::store::keys;
    let mut workspace = workspace.to_path_buf();
    let mut session = session.map(str::to_owned);
    // Subagents nest a few levels at most.
    for _ in 0..8 {
        let Some(sid) = session.take() else {
            break;
        };
        if let Some(source) = store
            .session_meta(&sid, keys::WORKTREE_SOURCE)
            .ok()
            .flatten()
        {
            return PathBuf::from(source);
        }
        let Some(parent) = store
            .session_meta(&sid, keys::SUBAGENT_PARENT)
            .ok()
            .flatten()
        else {
            break;
        };
        let Some(path) = store
            .session(&parent)
            .ok()
            .flatten()
            .and_then(|row| row["workspace"].as_str().map(PathBuf::from))
        else {
            break;
        };
        workspace = path;
        session = Some(parent);
    }
    workspace
}

fn providers(store: &Store, session: &str, key: &str) -> BTreeSet<String> {
    store
        .session_meta(session, key)
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .map(|list| list.into_iter().collect())
        .unwrap_or_default()
}

fn add_providers<'a>(
    store: &Store,
    session: &str,
    key: &str,
    new: impl IntoIterator<Item = &'a String>,
) -> Result<()> {
    let mut all = providers(store, session, key);
    let before = all.len();
    all.extend(new.into_iter().cloned());
    if all.len() == before && store.session_meta(session, key)?.is_some() {
        return Ok(());
    }
    let list: Vec<&String> = all.iter().collect();
    store.set_session_meta(session, key, &serde_json::to_string(&list)?)
}

/// The providers this conversation agreed may receive its work as a role.
pub fn consented(store: &Store, session: &str) -> BTreeSet<String> {
    providers(store, session, CONSENT_META)
}

/// Remember providers the user allowed in the consent dialog.
pub fn record_consent<'a>(
    store: &Store,
    session: &str,
    providers: impl IntoIterator<Item = &'a String>,
) -> Result<()> {
    add_providers(store, session, CONSENT_META, providers)
}

/// The providers that received this conversation as a role while it ran in
/// the cloud ([`SEEN_META`]).
pub fn seen(store: &Store, session: &str) -> BTreeSet<String> {
    providers(store, session, SEEN_META)
}

pub fn record_seen<'a>(
    store: &Store,
    session: &str,
    providers: impl IntoIterator<Item = &'a String>,
) -> Result<()> {
    add_providers(store, session, SEEN_META, providers)
}

/// Where one role runs.
#[derive(Clone, Debug)]
pub struct Target {
    pub role: Role,
    /// The saved setting: `""` when the role uses the conversation's model.
    pub setting: String,
    pub model: ModelConfig,
}

impl Target {
    pub fn vendor(&self) -> Option<Vendor> {
        Vendor::from_provider(&self.model.provider)
    }
    /// Runs on this computer (the managed runtime or a loopback endpoint).
    pub fn local(&self) -> bool {
        handoff::is_local(&self.model)
    }
    /// Uses the managed llama.cpp runtime (one local model at a time).
    pub fn managed_local(&self) -> bool {
        crate::local_engine::is_managed(&self.model)
    }
    /// `vendor` (a subscription CLI runs its own agent loop) or `shadowcode`.
    pub fn runner(&self) -> &'static str {
        if self.vendor().is_some() {
            "vendor"
        } else {
            "shadowcode"
        }
    }
    /// How the role is paid for: `local` (no cost), `subscription` (a vendor
    /// plan) or `api` (billed per token).
    pub fn cost(&self) -> &'static str {
        if self.local() {
            "local"
        } else if self.vendor().is_some() {
            "subscription"
        } else {
            "api"
        }
    }
    pub fn name(&self) -> String {
        model_label(&self.model)
    }
    pub fn to_json(&self) -> Value {
        json!({
            "role": self.role.id(),
            "label": self.role.label(),
            "setting": self.setting,
            "id": self.model.default,
            "name": self.name(),
            "provider": self.model.provider,
            "local": self.local(),
            "runner": self.runner(),
            "vendor": self.vendor().map(Vendor::id),
            "cost": self.cost(),
        })
    }
}

/// "Claude Code", "Codex · gpt-5.6-luna", "Qwen3 14B".
pub fn model_label(model: &ModelConfig) -> String {
    match Vendor::from_provider(&model.provider) {
        Some(vendor) if model.name.is_empty() || model.name == "default" => {
            vendor.product_label().into()
        }
        Some(vendor) => format!("{} · {}", vendor.product_label(), model.name),
        None if model.name.is_empty() => model.default.clone(),
        None => model.name.clone(),
    }
}

/// Resolve one role's setting. `""` and `skip` use the conversation's model.
pub fn resolve(
    store: &Store,
    setup: &Setup,
    role: Role,
    conversation: &ModelConfig,
) -> Result<Target> {
    let setting = setup.get(role).trim();
    let model = match setting {
        "" | SKIP => conversation.clone(),
        id => crate::model_registry::resolve(store, id, conversation).with_context(|| {
            format!(
                "The {} role's model ({id}) is not available. Choose another one in Settings › Roles.",
                role.id()
            )
        })?,
    };
    ensure!(
        model.provider != "mock",
        "Choose a local or compatible model for the {} role",
        role.id()
    );
    Ok(Target {
        role,
        setting: if setting == SKIP {
            String::new()
        } else {
            setting.to_owned()
        },
        model,
    })
}

/// Why a role cannot run for this conversation, if it cannot.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Offline, or the vendor is turned off: never allowed.
    Blocked(String),
    /// The conversation runs on this computer and this cloud provider has not
    /// been allowed yet.
    NeedsConsent,
}

/// The checks every role run passes (Plan → Implement → Review and
/// subagents alike).
pub struct Guard<'a> {
    pub config: &'a Config,
    /// The conversation runs on this computer.
    pub conversation_local: bool,
    pub consented: &'a BTreeSet<String>,
}

impl Guard<'_> {
    pub fn check(&self, target: &Target) -> std::result::Result<(), Refusal> {
        if target.local() {
            return Ok(());
        }
        let name = target.name();
        if self.config.offline() {
            return Err(Refusal::Blocked(format!(
                "Offline mode: the {} role uses {name}, which runs in the cloud. Choose a model on this computer for it in Settings › Roles, or go online.",
                target.role.id()
            )));
        }
        if let Some(vendor) = target.vendor() {
            if !self.config.cli_agents.vendor_enabled(vendor) {
                return Err(Refusal::Blocked(format!(
                    "The {} role uses {}, which is turned off in Settings › Advanced.",
                    target.role.id(),
                    vendor.product_label()
                )));
            }
        }
        if self.conversation_local && !self.consented.contains(&target.model.provider) {
            return Err(Refusal::NeedsConsent);
        }
        Ok(())
    }
}

/// The steps of one Plan → Implement → Review task, in order.
#[derive(Clone, Debug)]
pub struct Pipeline {
    pub stages: Vec<Target>,
}

impl Pipeline {
    /// The stages for a task in `mode` (`code` or `plan`).
    pub fn build(
        store: &Store,
        setup: &Setup,
        conversation: &ModelConfig,
        mode: &str,
    ) -> Result<Self> {
        let wanted: &[Role] = match mode {
            "code" => &[Role::Plan, Role::Implement, Role::Review],
            "plan" => &[Role::Plan],
            _ => bail!("Plan → Implement → Review runs Code and Plan tasks only"),
        };
        let mut stages = Vec::new();
        for role in wanted {
            if setup.get(*role).trim() == SKIP && mode == "code" {
                continue;
            }
            stages.push(resolve(store, setup, *role, conversation)?);
        }
        Ok(Self { stages })
    }
    pub fn stage(&self, role: Role) -> Option<&Target> {
        self.stages.iter().find(|s| s.role == role)
    }
    /// Every stage runs on this computer.
    pub fn local(&self) -> bool {
        self.stages.iter().all(Target::local)
    }
    /// Some stage uses the managed local runtime.
    pub fn uses_managed_local(&self) -> bool {
        self.stages.iter().any(Target::managed_local)
    }
    /// Cloud providers the stages send work to, without duplicates.
    pub fn cloud_providers(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        self.stages
            .iter()
            .filter(|s| !s.local())
            .map(|s| s.model.provider.clone())
            .filter(|p| seen.insert(p.clone()))
            .collect()
    }
    /// "Roles: Claude Code → Codex → Qwen3 14B".
    pub fn label(&self) -> String {
        let names: Vec<String> = self.stages.iter().map(Target::name).collect();
        truncate(&format!("Roles: {}", names.join(" → ")), 480).into()
    }
    pub fn to_json(&self) -> Value {
        json!(self.stages.iter().map(Target::to_json).collect::<Vec<_>>())
    }
    /// The routing decision recorded on the task.
    pub fn decision(&self) -> crate::routing::Decision {
        crate::routing::Decision {
            purpose: "roles".into(),
            source: "roles".into(),
            requested: "roles".into(),
            model_id: format!(
                "roles:{}",
                self.stages
                    .iter()
                    .map(|s| format!("{}={}", s.role.id(), s.model.default))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            model_name: self.label(),
            provider: PROVIDER.into(),
            context_limit: 0,
            fallback_reason: None,
            inference: if self.local() { "local" } else { "cloud" }.into(),
            route: "roles".into(),
            local_roles: Some(
                self.stages
                    .iter()
                    .filter(|s| s.local())
                    .map(|s| s.role.id().to_owned())
                    .collect(),
            ),
        }
    }
}

/// `{from, to, excerpt_chars, images, reason, roles}` for the consent dialog.
pub fn consent_request(
    pipeline_or_targets: &[Target],
    conversation: &str,
    excerpt_chars: usize,
) -> crate::cli_agent::handoff::ConsentRequired {
    let cloud: Vec<&Target> = pipeline_or_targets.iter().filter(|t| !t.local()).collect();
    let names: Vec<String> = cloud
        .iter()
        .map(|t| format!("the {} role ({})", t.role.id(), t.name()))
        .collect();
    let reason = format!(
        "this conversation runs on this computer ({conversation}); {} {} in the cloud and will receive your request{} and read the project's files",
        join_and(&names),
        if names.len() == 1 { "runs" } else { "run" },
        if excerpt_chars > 0 {
            ", a summary of this conversation"
        } else {
            ""
        }
    );
    let mut providers = BTreeSet::new();
    let to: Vec<String> = cloud
        .iter()
        .map(|t| t.name())
        .filter(|name| providers.insert(name.clone()))
        .collect();
    crate::cli_agent::handoff::ConsentRequired {
        handoff: json!({
            "from": conversation,
            "to": join_and(&to),
            "excerpt_chars": excerpt_chars,
            "images": 0,
            "reason": reason,
            "roles": cloud.iter().map(|t| json!({"role": t.role.id(), "label": t.role.label(), "name": t.name(), "provider": t.model.provider})).collect::<Vec<_>>(),
        }),
        reason,
    }
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The role's standing instructions, given to every runner (vendor CLIs get
/// them in the prompt; ShadowCode's own loop also in its system prompt).
pub fn instructions(role: Role) -> &'static str {
    match role {
        Role::Plan => "You are the plan role of a Plan → Implement → Review task in ShadowCode. Inspect the project and write a concrete implementation plan for the request: numbered steps, the files and functions each step changes, risks, and how to verify the result. Do not change any files. Another model implements your plan, so make it self-contained.",
        Role::Implement => "You are the implement role of a Plan → Implement → Review task in ShadowCode. Make the change the request asks for, following the plan when there is one. You work in an isolated copy of the project; your changes come back as a diff that another model reviews before it is applied. Keep the change focused, run the narrowest relevant check when you can, and finish with a short account of what you changed and what you verified.",
        Role::Review => "You are the review role of a Plan → Implement → Review task in ShadowCode. Review the diff below against the request and the plan. The diff is not applied yet: the project's files are still as they were before it. Report only concrete, supported findings: file and line, what is wrong and why. Separate real bugs from style notes. Do not change any files. End with exactly one line: `Verdict: ready` if the change can be applied as it is, or `Verdict: needs changes`.",
        Role::Explore => "You are the explore role in ShadowCode. Search and read the project to answer the question. Do not change files. Answer with exact paths and line numbers.",
    }
}

fn prior_block(prior: Option<&str>) -> String {
    match prior {
        Some(text) if !text.trim().is_empty() => format!("{text}\n\n"),
        _ => String::new(),
    }
}

pub fn plan_prompt(request: &str, prior: Option<&str>) -> String {
    format!(
        "{}{}\n\n<request>\n{}\n</request>",
        prior_block(prior),
        instructions(Role::Plan),
        truncate(request, REQUEST_FOR_ROLE)
    )
}

pub fn implement_prompt(request: &str, prior: Option<&str>, plan: Option<(&str, &str)>) -> String {
    let plan = match plan {
        Some((who, text)) if !text.trim().is_empty() => format!(
            "\n\n<plan from=\"{who}\">\n{}\n</plan>",
            truncate(text, PLAN_FOR_ROLE)
        ),
        _ => String::new(),
    };
    format!(
        "{}{}\n\n<request>\n{}\n</request>{plan}",
        prior_block(prior),
        instructions(Role::Implement),
        truncate(request, REQUEST_FOR_ROLE)
    )
}

pub fn review_prompt(
    request: &str,
    plan: Option<(&str, &str)>,
    implementer: &str,
    files: &[String],
    diff: &str,
) -> String {
    let plan = match plan {
        Some((who, text)) if !text.trim().is_empty() => format!(
            "\n\n<plan from=\"{who}\">\n{}\n</plan>",
            truncate(text, PLAN_FOR_ROLE)
        ),
        _ => String::new(),
    };
    let shown = truncate(diff, DIFF_FOR_REVIEW);
    let cut = if shown.len() < diff.len() {
        "\n(The diff was cut here; read the changed files' current versions and the plan for the rest.)"
    } else {
        ""
    };
    format!(
        "{}\n\n<request>\n{}\n</request>{plan}\n\n<changed_files by=\"{implementer}\">\n{}\n</changed_files>\n\n<diff>\n{shown}{cut}\n</diff>",
        instructions(Role::Review),
        truncate(request, REQUEST_FOR_ROLE),
        files.join("\n"),
    )
}

/// `ready` or `needs_changes` from the review's last `Verdict:` line.
pub fn verdict(review: &str) -> Option<&'static str> {
    review.lines().rev().find_map(|line| {
        let line = line
            .trim()
            .trim_matches(|c: char| c == '*' || c == '`' || c == '_')
            .to_ascii_lowercase();
        let rest = line.strip_prefix("verdict:")?.trim();
        let rest = rest.trim_matches(|c: char| c == '*' || c == '`' || c == '.' || c == ' ');
        if rest.starts_with("ready") || rest.starts_with("approve") {
            Some("ready")
        } else if rest.starts_with("needs") || rest.starts_with("changes") {
            Some("needs_changes")
        } else {
            None
        }
    })
}

/// `GET /api/roles`: the project's setup, each role resolved for the
/// conversation, presets, and what would stop a role from running.
pub fn view(
    store: &Store,
    config: &Config,
    project: &Path,
    session: Option<&str>,
    conversation: &ModelConfig,
) -> Result<Value> {
    let setup = load(store, project)?;
    let consented = session.map(|s| consented(store, s)).unwrap_or_default();
    let conversation_local = handoff::is_local(conversation);
    let guard = Guard {
        config,
        conversation_local,
        consented: &consented,
    };
    let mut roles = serde_json::Map::new();
    for role in Role::ALL {
        let value = match resolve(store, &setup, role, conversation) {
            Ok(target) => {
                let mut value = target.to_json();
                value["skipped"] = json!(setup.get(role) == SKIP);
                match guard.check(&target) {
                    Ok(()) => {}
                    Err(Refusal::Blocked(reason)) => value["blocked"] = json!(reason),
                    Err(Refusal::NeedsConsent) => value["needs_consent"] = json!(true),
                }
                value
            }
            Err(error) => json!({
                "role": role.id(),
                "label": role.label(),
                "setting": setup.get(role),
                "skipped": false,
                "blocked": format!("{error:#}"),
            }),
        };
        roles.insert(role.id().into(), value);
    }
    Ok(json!({
        "workspace": project,
        "setup": setup,
        "roles": roles,
        "presets": PRESETS.iter().map(|p| json!({
            "id": p.id,
            "label": p.label,
            "description": p.description,
            "roles": {"plan": p.roles[0], "implement": p.roles[1], "review": p.roles[2], "explore": p.roles[3]},
        })).collect::<Vec<_>>(),
        "conversation": {
            "id": conversation.default,
            "name": model_label(conversation),
            "local": conversation_local,
        },
        "offline": config.offline(),
        "consented": consented,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider: &str, default: &str, name: &str, endpoint: &str) -> ModelConfig {
        ModelConfig {
            provider: provider.into(),
            default: default.into(),
            name: name.into(),
            endpoint: endpoint.into(),
            ..ModelConfig::default()
        }
    }

    fn target(role: Role, model: ModelConfig) -> Target {
        Target {
            role,
            setting: model.default.clone(),
            model,
        }
    }

    #[test]
    fn roles_parse_and_map_agents() {
        assert_eq!(Role::parse("implement"), Some(Role::Implement));
        assert_eq!(Role::parse("nope"), None);
        let builtins = crate::agents::builtins();
        let by = |name: &str| Role::for_agent(builtins.iter().find(|a| a.name == name).unwrap());
        assert_eq!(by("explore"), Some(Role::Explore));
        assert_eq!(by("plan"), Some(Role::Plan));
        assert_eq!(by("review"), Some(Role::Review));
        assert_eq!(by("general"), Some(Role::Implement));
    }

    #[test]
    fn setups_validate_skips_and_placeholders() {
        let mut setup = Setup {
            plan: SKIP.into(),
            review: SKIP.into(),
            ..Setup::default()
        };
        setup.validate().unwrap();
        assert!(!setup.any());
        setup.implement = SKIP.into();
        assert!(setup.validate().is_err(), "implement cannot be skipped");
        setup.implement = LOCAL.into();
        assert!(setup.validate().is_err(), "presets resolve `local` first");
        setup.implement = "cli:codex".into();
        setup.validate().unwrap();
        assert!(setup.any());
        setup.plan = "x\ny".into();
        assert!(setup.validate().is_err());
    }

    #[test]
    fn presets_need_a_local_model_for_local_roles() {
        let mut setup = Setup::default();
        let claude = preset("claude-codex-local").unwrap();
        assert!(apply_preset(&mut setup, claude, None).is_err());
        apply_preset(&mut setup, claude, Some("local:gguf:q")).unwrap();
        assert_eq!(setup.plan, "cli:claude");
        assert_eq!(setup.implement, "cli:codex");
        assert_eq!(setup.review, "local:gguf:q");
        assert_eq!(setup.explore, "");
        assert_eq!(setup.preset, "claude-codex-local");
        setup.validate().unwrap();
        let plans = preset("claude-plans").unwrap();
        apply_preset(&mut setup, plans, None).unwrap();
        assert_eq!(setup.review, SKIP);
        assert!(PRESETS.iter().all(|p| p.label.len() < 80));
    }

    #[test]
    fn guard_refuses_cloud_offline_and_asks_consent_on_local_conversations() {
        let local = target(Role::Review, model("llamacpp", "local:gguf:q", "Qwen", ""));
        let claude = target(
            Role::Plan,
            crate::cli_agent::resolve_vendor("cli:claude").unwrap(),
        );
        let mut config = Config::default();
        let none = BTreeSet::new();
        let guard = Guard {
            config: &config,
            conversation_local: true,
            consented: &none,
        };
        assert_eq!(guard.check(&local), Ok(()));
        assert_eq!(guard.check(&claude), Err(Refusal::NeedsConsent));
        let allowed: BTreeSet<String> = ["cli:claude".to_owned()].into();
        let guard = Guard {
            config: &config,
            conversation_local: true,
            consented: &allowed,
        };
        assert_eq!(guard.check(&claude), Ok(()));
        // A cloud conversation already sends its work to the cloud.
        let guard = Guard {
            config: &config,
            conversation_local: false,
            consented: &none,
        };
        assert_eq!(guard.check(&claude), Ok(()));
        // Offline: refused whatever was consented.
        config.network.mode = crate::config::NetworkMode::Offline;
        let guard = Guard {
            config: &config,
            conversation_local: false,
            consented: &allowed,
        };
        match guard.check(&claude) {
            Err(Refusal::Blocked(reason)) => {
                assert!(reason.contains("Offline mode"), "{reason}");
                assert!(reason.contains("plan role"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(guard.check(&local), Ok(()));
        // A vendor turned off in Settings is refused.
        let mut config = Config::default();
        config.cli_agents.claude_enabled = false;
        let guard = Guard {
            config: &config,
            conversation_local: false,
            consented: &none,
        };
        assert!(matches!(guard.check(&claude), Err(Refusal::Blocked(_))));
    }

    #[test]
    fn pipelines_label_route_and_list_cloud_providers() {
        let pipeline = Pipeline {
            stages: vec![
                target(
                    Role::Plan,
                    crate::cli_agent::resolve_vendor("cli:claude").unwrap(),
                ),
                target(
                    Role::Implement,
                    crate::cli_agent::resolve_vendor("cli:codex:gpt-5.6-luna").unwrap(),
                ),
                target(
                    Role::Review,
                    model("llamacpp", "local:gguf:q", "Qwen3 14B", ""),
                ),
            ],
        };
        assert_eq!(
            pipeline.label(),
            "Roles: Claude Code → Codex · gpt-5.6-luna → Qwen3 14B"
        );
        assert!(!pipeline.local());
        assert!(pipeline.uses_managed_local());
        assert_eq!(pipeline.cloud_providers(), vec!["cli:claude", "cli:codex"]);
        let decision = pipeline.decision();
        assert_eq!(decision.provider, PROVIDER);
        assert_eq!(decision.inference, "cloud");
        // The next single-model turn sees a provider change and a handoff.
        let job =
            json!({"status":"completed","mode":"code","task":"t","summary":"s","routing":decision});
        let route = handoff::TurnRoute::of_job(&job).unwrap();
        assert_eq!(route.provider, PROVIDER);
        assert!(!route.local);
        let consent = consent_request(&pipeline.stages, "Qwen3 14B", 120);
        assert!(consent.reason.contains("the plan role (Claude Code)"));
        assert!(consent.reason.contains("a summary of this conversation"));
        assert_eq!(consent.handoff["roles"].as_array().unwrap().len(), 2);
        assert_eq!(
            consent.handoff["to"],
            "Claude Code and Codex · gpt-5.6-luna"
        );
    }

    #[test]
    fn prompts_are_bounded_and_carry_plan_and_diff() {
        let request = "r".repeat(40_000);
        let plan = "p".repeat(20_000);
        let prompt = implement_prompt(
            &request,
            Some("<prior_conversation/>"),
            Some(("Claude Code", &plan)),
        );
        assert!(prompt.starts_with("<prior_conversation/>"));
        assert!(prompt.contains("<plan from=\"Claude Code\">"));
        assert!(prompt.len() < REQUEST_FOR_ROLE + PLAN_FOR_ROLE + 2_000);
        let diff = "d".repeat(60_000);
        let review = review_prompt(
            "fix",
            Some(("Claude Code", "1. do it")),
            "Codex",
            &["M a.rs".into()],
            &diff,
        );
        assert!(review.contains("Verdict: ready"));
        assert!(review.contains("M a.rs"));
        assert!(review.contains("The diff was cut here"));
        assert!(review.len() < DIFF_FOR_REVIEW + 4_000);
        let plan_only = plan_prompt("add X", None);
        assert!(plan_only.contains("<request>\nadd X\n</request>"));
        assert!(plan_only.contains("Do not change any files"));
    }

    #[test]
    fn subagent_and_worktree_conversations_belong_to_their_project() {
        use crate::store::keys;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db")).unwrap();
        let project = dir.path().join("project");
        let session = |folder: &Path| {
            store.create_session(folder, "m", "").unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let parent = session(&project);
        assert_eq!(project_of(&store, &project, Some(&parent)), project);
        assert_eq!(project_of(&store, &project, None), project);
        // A write subagent in its worktree, and one it started in another.
        let worktree = dir.path().join("worktree");
        let child = session(&worktree);
        store
            .set_session_meta(&child, keys::SUBAGENT_PARENT, &parent)
            .unwrap();
        assert_eq!(project_of(&store, &worktree, Some(&child)), project);
        let nested = dir.path().join("nested");
        let grandchild = session(&nested);
        store
            .set_session_meta(&grandchild, keys::SUBAGENT_PARENT, &child)
            .unwrap();
        assert_eq!(project_of(&store, &nested, Some(&grandchild)), project);
        // A worktree task, and a subagent of it.
        let task = session(&worktree);
        store
            .set_session_meta(&task, keys::WORKTREE_SOURCE, &project.to_string_lossy())
            .unwrap();
        assert_eq!(project_of(&store, &worktree, Some(&task)), project);
        let helper = session(&nested);
        store
            .set_session_meta(&helper, keys::SUBAGENT_PARENT, &task)
            .unwrap();
        assert_eq!(project_of(&store, &nested, Some(&helper)), project);
    }

    #[test]
    fn verdicts_are_read_from_the_last_verdict_line() {
        assert_eq!(verdict("Looks fine.\nVerdict: ready"), Some("ready"));
        assert_eq!(
            verdict("**Verdict: needs changes**\n"),
            Some("needs_changes")
        );
        assert_eq!(verdict("`Verdict: Ready.`"), Some("ready"));
        assert_eq!(
            verdict("Verdict: ready\n\nVerdict: needs changes"),
            Some("needs_changes")
        );
        assert_eq!(verdict("no verdict here"), None);
    }
}

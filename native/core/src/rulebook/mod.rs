//! One rulebook for every agent.
//!
//! The user's profile folder (`~/.config/shadowcode/profile/`, following
//! `XDG_CONFIG_HOME`; `<--profile>/shadowcode/profile/` for an isolated
//! profile) holds `AGENTS.md`, `skills/`, `commands/` and `agents/`, plus
//! Git imports under `imports/<name>/` with the same layout. It is merged
//! with the project's own files: profile first, then project. When both
//! define a skill, command or agent with the same name, the project's (more
//! specific) one wins and the conflict is reported. Within the profile, the
//! user's own files win over imports.
//!
//! Every item can be switched off on its own (`rulebook.json` in the
//! settings folder). Profile text reaches agents labelled as the user's
//! instructions; project text stays labelled as repository content. Neither
//! grants permissions: approvals, trust, sandbox and read-only mode are
//! decided elsewhere and never read these files.
//!
//! Files are read through a directory capability rooted at the profile
//! folder, so a symlink inside it cannot reach a file outside it.
use crate::{
    instructions::{self, RootFile},
    paths::AppPaths,
    tools::truncate,
    workflows::{self, Catalog, Definition},
    workspace::Workspace,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::Mutex,
};

pub mod check;
pub mod delivery;
pub mod export;
pub mod import;
pub mod starters;

pub use delivery::VendorRules;

/// All profile rules together: half of the existing root guidance budget
/// (48 KB), so the project's own guidance always keeps the other half.
pub const PROFILE_TOTAL_BYTES: usize = instructions::ROOT_TOTAL_BYTES / 2;
/// One profile rules file is cut at this size, so a full-size file and its
/// label always fit the profile's share.
pub const PROFILE_FILE_BYTES: usize = 16_000;
/// Skills listed for a model, and the bytes the list may use.
pub const SKILL_INDEX_ENTRIES: usize = 48;
pub const SKILL_INDEX_BYTES: usize = 6000;
/// A profile `AGENTS.md` the app edits is refused above this size.
pub const MAX_RULES_WRITE: usize = 64_000;
pub const MAX_IMPORTS: usize = 8;
const STATE_FILE: &str = "rulebook.json";
const MAX_DISABLED: usize = 4096;
const MAX_PROJECTS: usize = 512;

/// `~/.config/shadowcode/profile` (next to the settings folder, like the
/// user agents folder).
pub fn profile_dir(paths: &AppPaths) -> PathBuf {
    paths
        .config
        .parent()
        .map(|p| p.join("shadowcode").join("profile"))
        .unwrap_or_else(|| paths.config.join("profile"))
}

/// Create the profile folder (mode 700) when the user first writes to it.
pub fn ensure_profile(paths: &AppPaths) -> Result<PathBuf> {
    let dir = profile_dir(paths);
    crate::paths::private_directory(&dir)?;
    Ok(dir)
}

/// A Git import recorded by `import::add`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ImportRecord {
    pub url: String,
    pub added_at: f64,
}

/// A link written by an export, removed again by `export::disable`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ExportLink {
    pub link: String,
    pub target: String,
}

/// Saved choices: switched-off items, vendor sharing, imports and exports.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct State {
    pub version: u32,
    /// Switched-off profile items (`profile:<path inside the profile>`).
    pub disabled: BTreeSet<String>,
    /// Switched-off project items per project folder (`project:<path>`).
    pub projects: BTreeMap<String, BTreeSet<String>>,
    /// Send the rulebook to vendor CLIs (Claude Code, Codex, Cursor, Grok,
    /// Antigravity). ShadowCode's own agent always reads it.
    pub share_with_cli_agents: bool,
    pub imports: BTreeMap<String, ImportRecord>,
    /// Export target (`claude`, `codex`) → links created.
    pub exports: BTreeMap<String, Vec<ExportLink>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            disabled: BTreeSet::new(),
            projects: BTreeMap::new(),
            share_with_cli_agents: true,
            imports: BTreeMap::new(),
            exports: BTreeMap::new(),
        }
    }
}

fn state_lock() -> &'static Mutex<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    &LOCK
}

impl State {
    pub fn file(paths: &AppPaths) -> PathBuf {
        paths.config.join(STATE_FILE)
    }
    /// The saved state; a missing file is the default.
    pub fn load(paths: &AppPaths) -> Result<Self> {
        let file = Self::file(paths);
        let text = match std::fs::read(&file) {
            Ok(bytes) => {
                ensure!(bytes.len() <= 1_000_000, "{STATE_FILE} exceeds 1 MB");
                String::from_utf8(bytes).context("rulebook.json is not UTF-8")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(error.into()),
        };
        let state: Self = serde_json::from_str(&text).with_context(|| {
            format!(
                "{} is not valid; fix it or remove it to reset rule choices",
                file.display()
            )
        })?;
        state.validate()?;
        Ok(state)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.disabled.len() <= MAX_DISABLED
                && self.projects.len() <= MAX_PROJECTS
                && self.projects.values().all(|v| v.len() <= MAX_DISABLED)
                && self.imports.len() <= MAX_IMPORTS
                && self.exports.values().all(|v| v.len() <= 256),
            "rulebook.json has too many entries"
        );
        Ok(())
    }
    /// Change the saved state under a lock and write it atomically (mode 600).
    pub fn update<T>(paths: &AppPaths, change: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let _guard = state_lock()
            .lock()
            .map_err(|_| anyhow::anyhow!("Rulebook lock poisoned"))?;
        let mut state = Self::load(paths)?;
        let result = change(&mut state)?;
        state.validate()?;
        crate::paths::atomic_write(
            &Self::file(paths),
            serde_json::to_string_pretty(&state)?.as_bytes(),
            true,
        )?;
        Ok(result)
    }
}

/// The id of a project item: `project:<path relative to the project>`.
pub fn project_id(path: &str) -> String {
    format!("project:{path}")
}
/// The id of a profile item: `profile:<path inside the profile folder>`.
pub fn profile_id(rel: &str) -> String {
    format!("profile:{rel}")
}

/// A rules file from the profile.
#[derive(Clone, Debug)]
pub struct ProfileRules {
    pub id: String,
    /// `profile` or `import:<name>`.
    pub source: String,
    /// Absolute path, shown to the user and the agent.
    pub path: String,
    pub content: String,
    pub hash: String,
    pub enabled: bool,
    /// The import's URL, for the label.
    pub origin: Option<String>,
}

/// One row of the Rules & skills page.
#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub id: String,
    /// `profile` or `project`.
    pub scope: String,
    /// `profile`, `import:<name>` or `project`.
    pub source: String,
    /// `rules`, `skill`, `command` or `agent`.
    pub kind: String,
    pub name: String,
    pub path: String,
    pub description: String,
    pub enabled: bool,
    pub bytes: usize,
    pub hash: String,
    /// A more specific item with the same name that is used instead.
    pub overridden_by: Option<String>,
}

/// The profile and saved choices, opened for one project (or none).
pub struct Book {
    pub dir: PathBuf,
    profile: Option<Workspace>,
    pub state: State,
    /// Import folders found under `imports/`, sorted, at most `MAX_IMPORTS`.
    pub imports: Vec<String>,
    /// Problems reading the profile or the saved state.
    pub issues: Vec<String>,
    project: Option<String>,
}

fn not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

/// A usable import folder name: letters, digits, `-`, `_`, `.` (not first).
pub fn valid_import_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl Book {
    /// Open the profile for `project` (the canonical project folder).
    pub fn load(paths: &AppPaths, project: Option<&Path>) -> Self {
        let dir = profile_dir(paths);
        let mut issues = Vec::new();
        let state = State::load(paths).unwrap_or_else(|error| {
            issues.push(format!("{error:#}"));
            State::default()
        });
        Self::open(dir, state, project, issues)
    }
    /// A book over an explicit folder and state (tests, previews).
    pub fn open(
        dir: PathBuf,
        state: State,
        project: Option<&Path>,
        mut issues: Vec<String>,
    ) -> Self {
        let profile = match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => match Workspace::open(&dir) {
                Ok(workspace) => Some(workspace),
                Err(error) => {
                    issues.push(format!("{}: {error:#}", dir.display()));
                    None
                }
            },
            Ok(_) => {
                issues.push(format!(
                    "{} is not a folder; ShadowCode reads profile rules only from a real folder",
                    dir.display()
                ));
                None
            }
            Err(_) => None,
        };
        let mut imports = Vec::new();
        if let Some(profile) = &profile {
            match profile.list("imports") {
                Ok(entries) => {
                    for entry in entries {
                        if entry.kind == "dir" && valid_import_name(&entry.name) {
                            imports.push(entry.name);
                        }
                    }
                }
                Err(error) if not_found(&error) => {}
                Err(error) => issues.push(format!("imports: {error:#}")),
            }
        }
        imports.sort();
        if imports.len() > MAX_IMPORTS {
            issues.push(format!(
                "Only the first {MAX_IMPORTS} imported profiles are read"
            ));
            imports.truncate(MAX_IMPORTS);
        }
        Self {
            dir,
            profile,
            state,
            imports,
            issues,
            project: project.map(|p| p.to_string_lossy().into_owned()),
        }
    }
    pub fn exists(&self) -> bool {
        self.profile.is_some()
    }
    /// Whether an item is switched on (items are on unless switched off).
    pub fn enabled(&self, id: &str) -> bool {
        if id.starts_with("project:") {
            !self
                .project
                .as_ref()
                .and_then(|p| self.state.projects.get(p))
                .is_some_and(|set| set.contains(id))
        } else {
            !self.state.disabled.contains(id)
        }
    }
    /// Profile sources in merge order: imports first, then the user's own
    /// files, so the user's own win. `(source, prefix inside the profile)`.
    fn sources(&self) -> Vec<(String, String)> {
        let mut sources: Vec<_> = self
            .imports
            .iter()
            .map(|name| (format!("import:{name}"), format!("imports/{name}/")))
            .collect();
        sources.push(("profile".into(), String::new()));
        sources
    }
    fn origin(&self, source: &str) -> Option<String> {
        source
            .strip_prefix("import:")
            .and_then(|name| self.state.imports.get(name))
            .map(|record| crate::redaction::redact_text(&record.url).text)
    }
    /// Relative path inside the profile of an absolute path it reported.
    pub fn relative(&self, path: &str) -> Option<String> {
        Path::new(path)
            .strip_prefix(&self.dir)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }
    pub fn path_of(&self, rel: &str) -> String {
        self.dir.join(rel).display().to_string()
    }
    /// Rules files: `AGENTS.md` of every source (an import without one may
    /// use `CLAUDE.md`). Imports first, the user's own file last.
    pub fn profile_rules(&self) -> Vec<ProfileRules> {
        let Some(profile) = &self.profile else {
            return Vec::new();
        };
        let mut rules = Vec::new();
        for (source, prefix) in self.sources() {
            let names: &[&str] = if prefix.is_empty() {
                &["AGENTS.md"]
            } else {
                &["AGENTS.md", "CLAUDE.md"]
            };
            for name in names {
                let rel = format!("{prefix}{name}");
                match profile.read(&rel) {
                    Ok(file) => {
                        let id = profile_id(&rel);
                        rules.push(ProfileRules {
                            enabled: self.enabled(&id),
                            id,
                            origin: self.origin(&source),
                            source: source.clone(),
                            path: self.path_of(&rel),
                            hash: file.hash,
                            content: file.content,
                        });
                        break;
                    }
                    Err(error) if not_found(&error) => {}
                    Err(_) => {}
                }
            }
        }
        rules
    }
    /// The user's own `AGENTS.md` (content and hash, `missing` when absent).
    pub fn own_rules(&self) -> (String, String) {
        self.profile
            .as_ref()
            .and_then(|p| p.read("AGENTS.md").ok())
            .map(|f| (f.content, f.hash))
            .unwrap_or_else(|| (String::new(), "missing".into()))
    }
    /// Every profile definition (skills and commands), all sources, before
    /// switches and conflicts are applied; plus discovery problems.
    pub fn profile_definitions(&self) -> (Vec<Definition>, Vec<String>) {
        let Some(profile) = &self.profile else {
            return (Vec::new(), Vec::new());
        };
        let mut definitions = Vec::new();
        let mut issues = Vec::new();
        for (source, prefix) in self.sources() {
            let catalog = workflows::discover_profile(profile, &prefix, &source);
            definitions.extend(catalog.definitions);
            issues.extend(catalog.issues);
        }
        (definitions, issues)
    }
    fn definition_id(&self, definition: &Definition) -> String {
        if definition.info.source == "project" {
            project_id(&definition.info.path)
        } else {
            profile_id(
                &self
                    .relative(&definition.info.path)
                    .unwrap_or_else(|| definition.info.path.clone()),
            )
        }
    }
    /// The merged catalog agents and slash commands use: enabled project
    /// definitions, then enabled profile ones whose name no project
    /// definition uses. Conflicts are reported as issues.
    pub fn catalog(&self, workspace: &Workspace) -> Catalog {
        let mut catalog = workflows::discover(workspace);
        catalog
            .definitions
            .retain(|d| self.enabled(&project_id(&d.info.path)));
        let (profile, issues) = self.profile_definitions();
        catalog.issues.extend(issues);
        let mut own: Vec<Definition> = Vec::new();
        // The user's own files come last in `sources`; walk in reverse so
        // they claim their names before imports do.
        for definition in profile.into_iter().rev() {
            if !self.enabled(&self.definition_id(&definition)) {
                continue;
            }
            let names = |d: &Definition| {
                let mut names = vec![d.info.name.clone()];
                if !d.alias.is_empty() {
                    names.push(d.alias.clone());
                }
                names
            };
            let clash = |d: &Definition| {
                d.info.kind == definition.info.kind
                    && names(d).iter().any(|n| names(&definition).contains(n))
            };
            if let Some(winner) = catalog.definitions.iter().find(|d| clash(d)) {
                catalog.issues.push(format!(
                    "Your profile {} /{} ({}) is not used here: the project's {} takes its place",
                    definition.info.kind,
                    definition.info.name,
                    definition.info.path,
                    winner.info.path
                ));
                continue;
            }
            if let Some(winner) = own.iter().find(|d| clash(d)) {
                catalog.issues.push(format!(
                    "{} /{} from {} is not used: {} has the same name",
                    definition.info.kind,
                    definition.info.name,
                    definition.info.path,
                    winner.info.path
                ));
                continue;
            }
            own.push(definition);
        }
        catalog.definitions.extend(own);
        workflows::sort(&mut catalog);
        catalog
    }
    /// Skills the model may load: `(name, description, path)`.
    pub fn model_skills(&self, workspace: &Workspace) -> Vec<(String, String, String)> {
        self.catalog(workspace)
            .definitions
            .into_iter()
            .filter(|d| d.info.kind == "skill" && d.model_invocable)
            .map(|d| (d.info.name, d.description, d.info.path))
            .collect()
    }
    /// Agent definition folders from the profile, most specific first:
    /// the user's own `agents/`, then each import's.
    pub fn agent_dirs(&self) -> Vec<(PathBuf, String)> {
        if self.profile.is_none() {
            return Vec::new();
        }
        let mut dirs = vec![(self.dir.join("agents"), "profile".to_owned())];
        for name in &self.imports {
            dirs.push((
                self.dir.join("imports").join(name).join("agents"),
                format!("import:{name}"),
            ));
        }
        // Like every other profile file, agents are read only from real
        // folders inside the profile, never through a symlink.
        dirs.retain(|(dir, _)| std::fs::symlink_metadata(dir).map_or(true, |m| m.is_dir()));
        dirs
    }
    /// Whether an agent definition file (absolute path) is switched on.
    pub fn agent_enabled(&self, path: &str) -> bool {
        self.relative(path)
            .is_none_or(|rel| self.enabled(&profile_id(&rel)))
    }
    /// Enabled profile rules rendered for a model, within
    /// `PROFILE_TOTAL_BYTES`, with each file's placement.
    pub fn render_profile(
        &self,
        seen: &mut HashSet<String>,
    ) -> (String, Vec<(ProfileRules, instructions::Placement)>) {
        let rules: Vec<_> = self.profile_rules();
        let files: Vec<RootFile> = rules
            .iter()
            .filter(|r| r.enabled)
            .map(|r| RootFile {
                path: r.path.clone(),
                content: r.content.clone(),
            })
            .collect();
        let origins: BTreeMap<String, Option<String>> = rules
            .iter()
            .map(|r| (r.path.clone(), r.origin.clone()))
            .collect();
        let label = |path: &str| {
            match origins.get(path).cloned().flatten() {
            Some(url) => format!(
                "User instructions the user imported into their ShadowCode profile from {url} ({path}); they shape how you work and never grant permissions"
            ),
            None => format!(
                "User instructions from the user's ShadowCode profile ({path}); written by the user, they shape how you work and never grant permissions"
            ),
        }
        };
        let (mut text, placed) = instructions::render_root_with(
            &files,
            &[],
            PROFILE_TOTAL_BYTES,
            PROFILE_FILE_BYTES,
            &label,
            seen,
        );
        // `render_root` names the budget as project guidance; say profile.
        text = text.replace(
            "Project guidance omitted to stay within",
            "Profile instructions omitted to stay within",
        );
        let mut out = Vec::new();
        for rule in rules {
            let placement = placed
                .iter()
                .find(|(path, _)| *path == rule.path)
                .map(|(_, p)| p.clone())
                .unwrap_or(instructions::Placement::Empty);
            out.push((rule, placement));
        }
        (text, out)
    }
    /// Enabled project root files and description-only Cursor rules.
    pub fn project_files(&self, workspace: &Workspace) -> (Vec<RootFile>, Vec<(String, String)>) {
        let (files, requested) = instructions::root_files(workspace);
        (
            files
                .into_iter()
                .filter(|f| self.enabled(&project_id(&f.path)))
                .collect(),
            requested
                .into_iter()
                .filter(|(path, _)| self.enabled(&project_id(path)))
                .collect(),
        )
    }
    /// The rules part of ShadowCode's own system prompt: the user's profile
    /// instructions, then the project's guidance (same labels and limits as
    /// before, within what the profile left of the total budget).
    pub fn guidance(&self, workspace: &Workspace) -> String {
        let mut seen = HashSet::new();
        let (profile, _) = self.render_profile(&mut seen);
        let (files, requested) = self.project_files(workspace);
        let budget = instructions::ROOT_TOTAL_BYTES.saturating_sub(profile.len());
        let (project, _) = instructions::render_root(
            &files,
            &requested,
            budget,
            &instructions::project_label,
            &mut seen,
        );
        let mut out = profile;
        if !out.is_empty() && !project.is_empty() {
            out.push_str(PRECEDENCE);
        }
        out.push_str(&project);
        out
    }
    /// Every row for the Rules & skills page, switched off or not.
    pub fn items(&self, workspace: Option<&Workspace>) -> Vec<Item> {
        let mut items = Vec::new();
        for rule in self.profile_rules() {
            items.push(Item {
                id: rule.id,
                scope: "profile".into(),
                source: rule.source,
                kind: "rules".into(),
                name: Path::new(&rule.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: rule.path,
                description: String::new(),
                enabled: rule.enabled,
                bytes: rule.content.len(),
                hash: rule.hash,
                overridden_by: None,
            });
        }
        let project = workspace.map(workflows::discover).unwrap_or_default();
        let (profile, _) = self.profile_definitions();
        let used = workspace
            .map(|w| self.catalog(w))
            .unwrap_or_else(|| Catalog {
                definitions: profile.clone(),
                ..Default::default()
            });
        for definition in &profile {
            let id = self.definition_id(definition);
            let overridden_by = (!used
                .definitions
                .iter()
                .any(|d| d.info.path == definition.info.path)
                && self.enabled(&id))
            .then(|| {
                used.definitions
                    .iter()
                    .find(|d| {
                        d.info.kind == definition.info.kind && d.info.name == definition.info.name
                    })
                    .map(|d| d.info.path.clone())
            })
            .flatten();
            items.push(Item {
                enabled: self.enabled(&id),
                id,
                scope: "profile".into(),
                source: definition.info.source.clone(),
                kind: definition.info.kind.clone(),
                name: definition.info.name.clone(),
                path: definition.info.path.clone(),
                description: definition.description.clone(),
                bytes: definition.raw_content.len(),
                hash: definition.info.hash.clone(),
                overridden_by,
            });
        }
        for dir in self.agent_dirs() {
            for (path, name, description) in agent_files(&dir.0) {
                let id = profile_id(&self.relative(&path).unwrap_or_else(|| path.clone()));
                items.push(Item {
                    enabled: self.enabled(&id),
                    id,
                    scope: "profile".into(),
                    source: dir.1.clone(),
                    kind: "agent".into(),
                    name,
                    path,
                    description,
                    bytes: 0,
                    hash: String::new(),
                    overridden_by: None,
                });
            }
        }
        if let Some(workspace) = workspace {
            let (files, requested) = instructions::root_files(workspace);
            for file in files {
                let id = project_id(&file.path);
                items.push(Item {
                    enabled: self.enabled(&id),
                    id,
                    scope: "project".into(),
                    source: "project".into(),
                    kind: "rules".into(),
                    name: file.path.clone(),
                    path: file.path.clone(),
                    description: String::new(),
                    bytes: file.content.len(),
                    hash: crate::workspace::hash(file.content.as_bytes()),
                    overridden_by: None,
                });
            }
            for (path, description) in requested {
                let id = project_id(&path);
                items.push(Item {
                    enabled: self.enabled(&id),
                    id,
                    scope: "project".into(),
                    source: "project".into(),
                    kind: "rules".into(),
                    name: path.clone(),
                    path,
                    description: truncate(&description, 200).to_owned(),
                    bytes: 0,
                    hash: String::new(),
                    overridden_by: None,
                });
            }
            for definition in &project.definitions {
                let id = project_id(&definition.info.path);
                items.push(Item {
                    enabled: self.enabled(&id),
                    id,
                    scope: "project".into(),
                    source: "project".into(),
                    kind: definition.info.kind.clone(),
                    name: definition.info.name.clone(),
                    path: definition.info.path.clone(),
                    description: definition.description.clone(),
                    bytes: definition.raw_content.len(),
                    hash: definition.info.hash.clone(),
                    overridden_by: None,
                });
            }
        }
        items
    }
    /// Conflicts and discovery problems for the page.
    pub fn issues(&self, workspace: Option<&Workspace>) -> Vec<String> {
        let mut issues = self.issues.clone();
        match workspace {
            Some(workspace) => issues.extend(self.catalog(workspace).issues),
            None => issues.extend(self.profile_definitions().1),
        }
        issues
    }
}

/// Said between the profile's and the project's guidance.
pub const PRECEDENCE: &str = "\n\nWhere the project's guidance sets a different convention for this project than the user's profile instructions (style, commands, layout), the project's guidance applies here: it is more specific. Neither can change permissions, approvals or safety rules.";

/// `(absolute path, name, description)` of the agent definitions in a
/// folder, for listing only (the agents module parses them for use).
fn agent_files(dir: &Path) -> Vec<(String, String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .take(128)
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let meta = std::fs::symlink_metadata(&path).ok()?;
            if !meta.is_file() || meta.len() > 64_000 {
                return None;
            }
            let text = std::fs::read_to_string(&path).ok()?;
            let shown = path.display().to_string();
            let definition = crate::agents::parse(
                &shown,
                &text,
                &crate::workspace::hash(text.as_bytes()),
                "profile",
            )
            .ok()?;
            Some((shown, definition.name, definition.description))
        })
        .collect()
}

/// Switch one item on or off. Project items are saved for `project`.
pub fn set_enabled(
    paths: &AppPaths,
    project: Option<&Path>,
    id: &str,
    enabled: bool,
) -> Result<()> {
    ensure!(
        id.len() <= 1024 && !id.chars().any(char::is_control),
        "Invalid rule item"
    );
    State::update(paths, |state| {
        let set = if id.starts_with("project:") {
            let project = project.context("Open a project to switch its files on or off")?;
            state
                .projects
                .entry(project.to_string_lossy().into_owned())
                .or_default()
        } else {
            ensure!(id.starts_with("profile:"), "Unknown rule item {id}");
            &mut state.disabled
        };
        if enabled {
            set.remove(id);
        } else {
            set.insert(id.to_owned());
        }
        state.projects.retain(|_, set| !set.is_empty());
        Ok(())
    })
}

/// Replace the profile's own `AGENTS.md`. `expected` is the hash the editor
/// loaded (`missing` for a new file); a changed file is refused. An empty
/// text removes nothing: it is saved as an empty file.
pub fn save_rules(paths: &AppPaths, content: &str, expected: &str) -> Result<String> {
    ensure!(
        content.len() <= MAX_RULES_WRITE,
        "Profile AGENTS.md is limited to {MAX_RULES_WRITE} bytes"
    );
    ensure!(!content.contains('\0'), "AGENTS.md must be text");
    let dir = ensure_profile(paths)?;
    let profile = Workspace::open(&dir)?;
    let expected = (!expected.is_empty()).then_some(expected);
    profile.write("AGENTS.md", content.as_bytes(), expected)
}

/// Summary for `GET /api/rules`.
pub fn overview(paths: &AppPaths, workspace: Option<&Workspace>) -> Value {
    let book = Book::load(paths, workspace.map(|w| w.path.as_path()));
    let (content, hash) = book.own_rules();
    let imports: Vec<Value> = book
        .imports
        .iter()
        .map(|name| {
            let record = book.state.imports.get(name).cloned().unwrap_or_default();
            json!({
                "name": name,
                "url": crate::redaction::redact_text(&record.url).text,
                "path": book.dir.join("imports").join(name),
            })
        })
        .collect();
    json!({
        "profile": {
            "path": book.dir,
            "exists": book.exists(),
            "agents_md": {"content": content, "hash": hash, "path": book.dir.join("AGENTS.md")},
        },
        "workspace": workspace.map(|w| w.path.clone()),
        "share_with_cli_agents": book.state.share_with_cli_agents,
        "items": book.items(workspace),
        "imports": imports,
        "issues": book.issues(workspace),
        "limits": {
            "profile_file_bytes": PROFILE_FILE_BYTES,
            "profile_total_bytes": PROFILE_TOTAL_BYTES,
            "total_bytes": instructions::ROOT_TOTAL_BYTES,
            "skill_index_entries": SKILL_INDEX_ENTRIES,
            "skill_index_bytes": SKILL_INDEX_BYTES,
        },
    })
}

#[cfg(test)]
mod tests;

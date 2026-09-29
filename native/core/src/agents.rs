//! Agent definitions for subagents: Markdown files with YAML front matter.
//!
//! Discovery only reads confined text. A definition can narrow a child's
//! tools, choose a model and ask for write mode; it can never widen the
//! parent's permissions, trust, or approval requirements.
//!
//! Search order (first definition of a name wins; later ones are listed as
//! shadowed): `.shadow/agents/`, `.shadowcode/agents/`, `.claude/agents/`,
//! `.opencode/agent/`, `.opencode/agents/` in the project, then the user's
//! profile (`~/.config/shadowcode/profile/agents/`), the older per-user
//! `~/.config/shadowcode/agents/`, imported profiles, then the built-ins
//! `explore`, `plan`, `review` and `general`.
use crate::{paths::AppPaths, workspace::Workspace};
use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub const PROJECT_DIRS: [&str; 5] = [
    ".shadow/agents",
    ".shadowcode/agents",
    ".claude/agents",
    ".opencode/agent",
    ".opencode/agents",
];
const MAX_FILE_BYTES: usize = 64_000;
const MAX_FILES: usize = 128;
const MAX_TURNS: usize = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentMode {
    #[default]
    ReadOnly,
    Write,
}

#[derive(Clone, Debug, Serialize)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    /// Model id from the ShadowCode picker; `None` uses the parent's model.
    pub model: Option<String>,
    /// Allowed native tool names; empty means every tool the mode allows.
    pub tools: Vec<String>,
    pub deny: Vec<String>,
    pub mode: AgentMode,
    pub max_turns: Option<usize>,
    #[serde(skip)]
    pub instructions: String,
    /// builtin | project | user
    pub source: String,
    pub path: String,
    pub hash: String,
    /// Front-matter fields that were read but have no effect here.
    pub ignored: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct AgentCatalog {
    pub agents: Vec<AgentDefinition>,
    /// Definitions hidden by an earlier one with the same name.
    pub shadowed: Vec<Value>,
    pub issues: Vec<String>,
}

impl AgentCatalog {
    pub fn get(&self, name: &str) -> Option<&AgentDefinition> {
        let name = name.trim();
        self.agents.iter().find(|a| a.name == name).or_else(|| {
            // `@agent-code-reviewer` (Claude Code's mention form).
            name.strip_prefix("agent-")
                .and_then(|short| self.agents.iter().find(|a| a.name == short))
        })
    }
    pub fn to_json(&self) -> Value {
        json!({
            "agents": self.agents.iter().map(AgentDefinition::to_json).collect::<Vec<_>>(),
            "shadowed": self.shadowed,
            "issues": self.issues,
            "dirs": PROJECT_DIRS,
        })
    }
}

impl AgentDefinition {
    pub fn to_json(&self) -> Value {
        let mut value = json!(self);
        value["instructions_preview"] = json!(crate::tools::truncate(&self.instructions, 400));
        value
    }
    pub fn read_only(&self) -> bool {
        self.mode == AgentMode::ReadOnly
    }
}

/// `~/.config/shadowcode/agents` (next to the profile's config directory).
pub fn user_dir(paths: &AppPaths) -> PathBuf {
    paths
        .config
        .parent()
        .map(|p| p.join("shadowcode").join("agents"))
        .unwrap_or_else(|| paths.config.join("agents"))
}

/// Claude Code / opencode tool names mapped to ShadowCode's native tools.
/// Native names and `mcp__server__tool` names pass through unchanged.
pub fn native_tools(name: &str) -> Vec<String> {
    let mapped: &[&str] = match name.trim().to_ascii_lowercase().as_str() {
        "read" => &["read_file", "view_image"],
        "write" => &["write_file", "create_directory"],
        "edit" | "multiedit" => &["edit_file", "apply_patch"],
        "patch" => &["apply_patch"],
        "glob" => &["search_files", "list_files"],
        "grep" => &["search_text", "search_symbol"],
        "ls" | "list" => &["list_files"],
        "bash" => &[
            "exec",
            "background_start",
            "background_list",
            "background_output",
            "background_stop",
        ],
        "webfetch" => &["web_fetch"],
        "websearch" => &["web_search"],
        "todowrite" | "todoread" => &["update_plan"],
        "task" => &["spawn_agent"],
        "skill" => &["load_skill"],
        _ => &[],
    };
    if mapped.is_empty() {
        vec![name.trim().to_owned()]
    } else {
        mapped.iter().map(|s| (*s).to_owned()).collect()
    }
}

/// Tools that change the workspace or run commands.
pub fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "write_file"
            | "edit_file"
            | "apply_patch"
            | "create_directory"
            | "move_file"
            | "delete_file"
            | "exec"
            | "background_start"
            | "background_stop"
            | "git_add"
            | "git_commit"
    )
}

fn names(value: &Value, field: &str) -> Result<Vec<String>> {
    let raw: Vec<String> = match value {
        Value::Null => Vec::new(),
        Value::String(text) => text
            .split([',', ' ', '\n'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .with_context(|| format!("Agent field '{field}' must list tool names"))
            })
            .collect::<Result<_>>()?,
        _ => bail!("Agent field '{field}' must be text or a list"),
    };
    ensure!(
        raw.len() <= 128,
        "Agent field '{field}' lists too many tools"
    );
    let mut out = BTreeSet::new();
    for name in raw {
        ensure!(
            !name.is_empty() && name.len() <= 200 && !name.chars().any(char::is_control),
            "Invalid tool name in '{field}'"
        );
        // `Bash(git diff:*)`-style patterns select the whole tool here.
        let base = name.split('(').next().unwrap_or(&name).trim();
        out.extend(native_tools(base));
    }
    Ok(out.into_iter().collect())
}

fn front_matter(text: &str) -> Result<(Value, String)> {
    let normalized = text.replace("\r\n", "\n");
    if let Some(rest) = normalized.strip_prefix("---\n") {
        let (header, body) = rest
            .split_once("\n---")
            .context("Unclosed agent front matter")?;
        ensure!(
            body.is_empty() || body.starts_with('\n'),
            "Front matter must end on its own line"
        );
        let metadata: Value = if header.trim().is_empty() {
            json!({})
        } else {
            serde_yaml_ng::from_str(header).context("Invalid agent front matter")?
        };
        ensure!(metadata.is_object(), "Agent front matter must be an object");
        Ok((metadata, body.trim().to_owned()))
    } else {
        Ok((json!({}), normalized.trim().to_owned()))
    }
}

/// Parse one definition file. `path` is shown to the user; its file stem is
/// the fallback name.
pub fn parse(path: &str, text: &str, hash: &str, source: &str) -> Result<AgentDefinition> {
    ensure!(
        text.len() <= MAX_FILE_BYTES,
        "Agent definition exceeds 64 KB"
    );
    let (meta, body) = front_matter(text)?;
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .context("Agent name missing")?;
    for field in ["name", "description", "model", "mode"] {
        ensure!(
            meta.get(field).is_none_or(Value::is_string),
            "Agent field '{field}' must be text"
        );
    }
    let name = meta["name"].as_str().unwrap_or(stem).trim().to_owned();
    ensure!(
        crate::workflows::valid_name(&name),
        "Agent name must use 1–80 letters, numbers, - or _"
    );
    ensure!(!body.is_empty(), "Agent instructions are empty");
    let description = meta["description"]
        .as_str()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            body.lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim_start_matches('#')
                .trim()
                .chars()
                .take(200)
                .collect()
        });
    ensure!(
        description.len() <= 1000,
        "Agent description exceeds 1000 bytes"
    );
    let model = meta["model"]
        .as_str()
        .map(str::trim)
        .filter(|m| !m.is_empty() && *m != "inherit")
        .map(str::to_owned);
    if let Some(model) = &model {
        ensure!(
            model.len() <= 1024 && !model.chars().any(char::is_control),
            "Invalid agent model"
        );
    }
    let mut ignored = Vec::new();
    let mut tools = Vec::new();
    let mut deny = names(
        meta.get("disallowedTools")
            .or_else(|| meta.get("disallowed_tools"))
            .or_else(|| meta.get("deny"))
            .unwrap_or(&Value::Null),
        "disallowedTools",
    )?;
    let mut wants_write = false;
    match meta.get("tools") {
        // opencode: `tools: {write: false, bash: false}`.
        Some(Value::Object(map)) => {
            for (key, enabled) in map {
                let enabled = enabled
                    .as_bool()
                    .context("Agent tools map values must be true or false")?;
                let mapped = native_tools(key);
                if enabled {
                    wants_write |= mapped.iter().any(|t| is_write_tool(t));
                } else {
                    deny.extend(mapped);
                }
            }
        }
        Some(value) => {
            tools = names(value, "tools")?;
            wants_write = tools.iter().any(|t| is_write_tool(t));
        }
        None => {}
    }
    deny.sort();
    deny.dedup();
    let mode = match meta["mode"].as_str().map(str::trim) {
        Some("write" | "read-write" | "read_write" | "edit") => AgentMode::Write,
        Some("read-only" | "readonly" | "read_only" | "read") => AgentMode::ReadOnly,
        // opencode's primary/subagent/all describe where an agent is offered.
        Some("primary" | "subagent" | "all") | None => {
            if wants_write {
                AgentMode::Write
            } else {
                AgentMode::ReadOnly
            }
        }
        Some(other) => bail!("Agent mode must be read-only or write, not '{other}'"),
    };
    let turns = meta
        .get("max_turns")
        .or_else(|| meta.get("maxTurns"))
        .or_else(|| meta.get("steps"));
    let max_turns = match turns {
        None | Some(Value::Null) => None,
        Some(value) => {
            let n = value
                .as_u64()
                .context("Agent max_turns must be a whole number")?;
            ensure!(
                (1..=MAX_TURNS as u64).contains(&n),
                "Agent max_turns must be between 1 and {MAX_TURNS}"
            );
            Some(n as usize)
        }
    };
    for key in meta.as_object().into_iter().flat_map(|m| m.keys()) {
        if !matches!(
            key.as_str(),
            "name"
                | "description"
                | "model"
                | "tools"
                | "disallowedTools"
                | "disallowed_tools"
                | "deny"
                | "mode"
                | "max_turns"
                | "maxTurns"
                | "steps"
        ) {
            ignored.push(key.clone());
        }
    }
    Ok(AgentDefinition {
        name,
        description,
        model,
        tools,
        deny,
        mode,
        max_turns,
        instructions: body,
        source: source.into(),
        path: path.into(),
        hash: hash.into(),
        ignored,
    })
}

fn builtin(
    name: &str,
    description: &str,
    mode: AgentMode,
    max_turns: usize,
    instructions: &str,
) -> AgentDefinition {
    AgentDefinition {
        name: name.into(),
        description: description.into(),
        model: None,
        tools: Vec::new(),
        deny: Vec::new(),
        mode,
        max_turns: Some(max_turns),
        instructions: instructions.into(),
        source: "builtin".into(),
        path: String::new(),
        hash: String::new(),
        ignored: Vec::new(),
    }
}

pub fn builtins() -> Vec<AgentDefinition> {
    vec![
        builtin(
            "explore",
            "Read-only codebase search. Finds files, symbols and how things work; answers with exact paths and line numbers.",
            AgentMode::ReadOnly,
            24,
            "You are an exploration subagent. Search and read the project to answer the question you were given. Prefer search_text, search_files, workspace_symbols and focused read_file ranges over reading whole directories. Do not change files. Answer concisely: list the relevant files with paths and line numbers, then the facts you found. Say clearly what you could not find.",
        ),
        builtin(
            "plan",
            "Read-only planner. Inspects the code and returns a step-by-step implementation plan with the files to change.",
            AgentMode::ReadOnly,
            24,
            "You are a planning subagent. Inspect the relevant code, then return a concrete implementation plan: numbered steps, the files and functions each step touches, risks, and how to verify the result. Do not change files. Keep the plan short and specific to this repository.",
        ),
        builtin(
            "review",
            "Read-only reviewer. Checks changes or files for concrete bugs, missing tests and risky edits.",
            AgentMode::ReadOnly,
            24,
            "You are a review subagent. Inspect the changes or files you were pointed at (git_diff and git_status show pending work). Report only concrete, supported findings: file and line, what is wrong, and why. Separate real bugs from style notes. Do not change files. If nothing is wrong, say so.",
        ),
        builtin(
            "general",
            "General-purpose worker for a self-contained task. Can edit files in its own isolated worktree; its diff comes back for review.",
            AgentMode::Write,
            40,
            "You are a general-purpose subagent working on one self-contained task. Inspect before editing, keep changes focused on the task, and run the narrowest relevant check when approvals allow. Finish with a short account of what you changed, what you verified, and anything left undone.",
        ),
    ]
}

fn not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

fn push(catalog: &mut AgentCatalog, definition: AgentDefinition) {
    if let Some(existing) = catalog.agents.iter().find(|a| a.name == definition.name) {
        catalog.shadowed.push(json!({
            "name": definition.name,
            "path": definition.path,
            "source": definition.source,
            "shadowed_by": if existing.path.is_empty() { existing.source.clone() } else { existing.path.clone() },
        }));
    } else {
        catalog.agents.push(definition);
    }
}

/// Discover definitions for a project. `user` is the per-user directory
/// (normally `user_dir(paths)`); built-ins are always present.
pub fn discover(workspace: &Workspace, user: Option<&Path>) -> AgentCatalog {
    let dirs: Vec<(PathBuf, String)> = user
        .map(|u| vec![(u.to_path_buf(), "user".to_owned())])
        .unwrap_or_default();
    discover_with(workspace, &dirs, &|_| true)
}

/// The catalog with the user's profile (`rulebook`): project folders, the
/// profile's `agents/`, the per-user folder, imported profiles, built-ins.
/// Switched-off profile definitions are left out.
pub fn discover_for(paths: &AppPaths, workspace: &Workspace) -> AgentCatalog {
    let book = crate::rulebook::Book::load(paths, Some(&workspace.path));
    let mut dirs = book.agent_dirs();
    // The per-user folder predates the profile; it ranks after the
    // profile's own `agents/` and before imports.
    let at = dirs.len().min(1);
    dirs.insert(at, (user_dir(paths), "user".to_owned()));
    discover_with(workspace, &dirs, &|path| book.agent_enabled(path))
}

/// Discover definitions for a project from its folders, then `dirs` in
/// order (`(folder, source)`), then the built-ins. `enabled` sees each
/// user-level file's path.
pub fn discover_with(
    workspace: &Workspace,
    dirs: &[(PathBuf, String)],
    enabled: &dyn Fn(&str) -> bool,
) -> AgentCatalog {
    let mut catalog = AgentCatalog::default();
    let mut files = 0usize;
    for dir in PROJECT_DIRS {
        match workspace.list(dir) {
            Ok(entries) => {
                for entry in entries {
                    if entry.kind != "file" || !entry.name.ends_with(".md") {
                        continue;
                    }
                    files += 1;
                    if files > MAX_FILES {
                        catalog
                            .issues
                            .push(format!("At most {MAX_FILES} agent files are loaded"));
                        break;
                    }
                    match workspace
                        .read(&entry.path)
                        .and_then(|file| parse(&entry.path, &file.content, &file.hash, "project"))
                    {
                        Ok(definition) => push(&mut catalog, definition),
                        Err(error) => catalog.issues.push(format!("{}: {error:#}", entry.path)),
                    }
                }
            }
            Err(error) if not_found(&error) => {}
            Err(error) => catalog.issues.push(format!("{dir}: {error:#}")),
        }
    }
    for (user, source) in dirs {
        match fs::read_dir(user) {
            Ok(entries) => {
                let mut paths: Vec<PathBuf> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "md"))
                    .take(MAX_FILES)
                    .collect();
                paths.sort();
                for path in paths {
                    let shown = path.display().to_string();
                    if !enabled(&shown) {
                        continue;
                    }
                    let loaded = (|| -> Result<AgentDefinition> {
                        let meta = fs::symlink_metadata(&path)?;
                        ensure!(meta.is_file(), "Not a regular file");
                        ensure!(
                            meta.len() as usize <= MAX_FILE_BYTES,
                            "Agent definition exceeds 64 KB"
                        );
                        let bytes = fs::read(&path)?;
                        let text = String::from_utf8(bytes).context("Agent file is not UTF-8")?;
                        let hash = crate::workspace::hash(text.as_bytes());
                        parse(&shown, &text, &hash, source)
                    })();
                    match loaded {
                        Ok(definition) => push(&mut catalog, definition),
                        Err(error) => catalog.issues.push(format!("{shown}: {error:#}")),
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => catalog.issues.push(format!("{}: {error}", user.display())),
        }
    }
    for definition in builtins() {
        push(&mut catalog, definition);
    }
    catalog
}

/// The first `@name` (or `@agent-name`) in a prompt that names a known
/// agent, and the prompt with that mention removed. Mentions must start the
/// prompt or follow whitespace, so e-mail addresses and `@scope/pkg` never match.
pub fn mention<'a>(task: &str, catalog: &'a AgentCatalog) -> Option<(&'a AgentDefinition, String)> {
    let bytes = task.as_bytes();
    let mut index = 0;
    while let Some(offset) = task[index..].find('@') {
        let at = index + offset;
        index = at + 1;
        if at > 0 && !bytes[at - 1].is_ascii_whitespace() {
            continue;
        }
        let rest = &task[at + 1..];
        let len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(rest.len());
        if len == 0 {
            continue;
        }
        // A path (`@src/main.rs`, `@notes.md`) or an address, not an agent.
        let mut after = rest[len..].chars();
        let (next, then) = (after.next(), after.next());
        if matches!(next, Some('/' | '@'))
            || (next == Some('.') && then.is_some_and(|c| c.is_ascii_alphanumeric()))
        {
            continue;
        }
        if let Some(definition) = catalog.get(&rest[..len]) {
            let remaining = format!("{}{}", &task[..at], &rest[len..]);
            let remaining = remaining.trim();
            let prompt = if remaining.is_empty() {
                task.trim().to_owned()
            } else {
                remaining.to_owned()
            };
            return Some((definition, prompt));
        }
    }
    None
}

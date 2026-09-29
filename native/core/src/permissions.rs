//! Native tool policy. Two user-facing modes (`ask`, `allow_edits`) on top of
//! the advanced level (`read_only`, `workspace`, `elevated`):
//!
//! - web tools: allowed only when the task's web flag is on and the network
//!   mode is online; otherwise denied with the reason.
//! - file edits inside the project: `ask` asks with a readable summary,
//!   `allow_edits` allows. Paths outside the project are refused by the
//!   workspace layer after symlink resolution, before anything asks.
//! - shell, delete, Git history, background processes: always ask.
//! - destructive Git (reset/clean): denied below `elevated`, asked at `elevated`.
//! - privileged commands (sudo/su/pkexec/doas/run0): denied unless allow_root,
//!   and then still asked. They never run without a prompt.
//! - network-reaching shell commands: denied when shell network is off or the
//!   app is offline.
use crate::config::{Config, PermissionLevel, PermissionsConfig};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
    Ask(String),
}

pub fn read_only(tool: &str) -> bool {
    matches!(
        tool,
        "system_info"
            | "list_files"
            | "read_file"
            | "search_files"
            | "search_text"
            | "search_symbol"
            | "workspace_symbols"
            | "goto_definition"
            | "find_references"
            | "get_diagnostics"
            | "get_type_signature"
            | "repo_map"
            | "search_code"
            | "mcp_sqlite_tables"
            | "mcp_sqlite_query"
            | "background_list"
            | "background_output"
            | "git_status"
            | "git_diff"
            | "git_log"
            | "update_plan"
            | "update_todos"
            | "web_fetch"
            | "web_search"
            // Subagents: a child's own permissions decide what it may do.
            | "spawn_agent"
            | "load_skill"
    )
}

pub fn web_tool(tool: &str) -> bool {
    matches!(tool, "web_fetch" | "web_search")
}

pub fn parallel_safe(tool: &str, args: &Value) -> bool {
    (read_only(tool) && !matches!(tool, "update_plan" | "update_todos"))
        || (tool == "git_branch" && args["create"].as_bool() != Some(true))
}

const PRIVILEGED: &[&str] = &["sudo", "doas", "su", "pkexec", "run0", "--privileged"];
const NETWORK: &[&str] = &[
    "curl", "wget", "ssh", "scp", "sftp", "rsync", "nc", "ncat", "netcat", "socat", "telnet",
    "ftp", "nmap", "pip", "pip3", "npm", "npx", "pnpm", "yarn", "uv", "uvx", "bunx", "pipx",
    "aria2c",
];
const DANGEROUS: &[&str] = &[
    "rm", "mkfs", "dd", "chmod", "chown", "reboot", "shutdown", "--force", "--hard",
];

fn words(command: &str) -> Vec<&str> {
    command
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .filter(|s| !s.is_empty())
        .collect()
}

/// Git commands that discard work or rewrite history. Lexical and
/// conservative: it can only add prompts, never remove them.
pub fn destructive_git(command: &str) -> bool {
    let words = words(command);
    let Some(start) = words.iter().position(|w| *w == "git") else {
        return false;
    };
    let rest = &words[start + 1..];
    let has = |w: &str| rest.contains(&w);
    has("reset")
        || has("clean")
        || has("rebase")
        || has("filter-branch")
        || has("filter-repo")
        || has("update-ref")
        || has("restore")
        || (has("checkout") && command.contains(" -- "))
        || (has("push")
            && (has("--force") || has("-f") || has("--force-with-lease") || has("--delete")))
        || (has("branch") && (has("-D") || has("--delete")))
        || (has("stash") && (has("drop") || has("clear")))
        || (has("reflog") && has("expire"))
        || (has("gc") && command.contains("--prune"))
}

/// A readable one-line description of a file edit, for the approval prompt.
pub fn edit_summary(tool: &str, args: &Value) -> String {
    let path = args["path"].as_str().unwrap_or("?");
    match tool {
        "write_file" => format!("Write {path}"),
        "edit_file" => format!("Edit {path}"),
        "create_directory" => format!("Create directory {path}"),
        "move_file" => format!(
            "Move {} to {}",
            args["src"].as_str().unwrap_or("?"),
            args["dest"].as_str().unwrap_or("?")
        ),
        "delete_file" => format!("Delete {path}"),
        "apply_patch" => {
            let patch = args["patch"]
                .as_str()
                .or_else(|| args["diff"].as_str())
                .unwrap_or("");
            let mut files: Vec<&str> = patch
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("+++ ")
                        .map(|p| p.trim().trim_start_matches("b/"))
                        .filter(|p| *p != "/dev/null")
                        .or_else(|| line.strip_prefix("*** Update File: "))
                        .or_else(|| line.strip_prefix("*** Add File: "))
                        .or_else(|| line.strip_prefix("*** Delete File: "))
                        .map(str::trim)
                })
                .collect();
            files.dedup();
            if files.is_empty() {
                if let Some(path) = args["path"].as_str() {
                    files.push(path);
                }
            }
            let shown: Vec<_> = files.iter().take(6).copied().collect();
            let more = files.len().saturating_sub(shown.len());
            if shown.is_empty() {
                "Apply a patch".into()
            } else if more > 0 {
                format!("Apply a patch to {} and {more} more", shown.join(", "))
            } else {
                format!("Apply a patch to {}", shown.join(", "))
            }
        }
        _ => format!("Change files with {tool}"),
    }
}

pub fn check(config: &PermissionsConfig, tool: &str, args: &Value) -> Decision {
    use Decision::*;
    if web_tool(tool) {
        return if config.web {
            Allow
        } else if config.offline {
            Deny("The app is offline; web tools are unavailable".into())
        } else {
            Deny("Web tools are off for this task. Turn on web for the task (and keep the network mode online) to let the agent fetch pages.".into())
        };
    }
    if read_only(tool) || (tool == "git_branch" && args["create"].as_bool() != Some(true)) {
        return Allow;
    }
    if config.level == PermissionLevel::ReadOnly {
        return Deny(format!("{tool} is unavailable in read-only mode"));
    }
    match tool {
        "write_file" | "edit_file" | "apply_patch" | "create_directory" | "move_file" => {
            if config.approve_edits() {
                Ask(edit_summary(tool, args))
            } else {
                Allow
            }
        }
        "delete_file" => Ask(edit_summary(tool, args)),
        "git_add" => Ask("Stage files; repository clean filters may execute commands".into()),
        "git_commit" | "git_checkout" | "git_branch" => {
            Ask("Change repository history or the active branch".into())
        }
        "git_reset" | "git_clean" => {
            if config.level != PermissionLevel::Elevated {
                Deny("Destructive Git operations require elevated permissions".into())
            } else {
                Ask("Destructive Git operation: uncommitted work can be lost".into())
            }
        }
        "background_stop" => Ask("Stop a managed background process in this project".into()),
        "exec" | "background_start" => {
            let command = args["command"].as_str().unwrap_or("");
            let words = words(command);
            if words.iter().any(|word| PRIVILEGED.contains(word)) {
                if !config.allow_root {
                    return Deny(
                        "Root and privileged commands (sudo, su, pkexec, doas) are disabled".into(),
                    );
                }
                return Ask(
                    "Privileged command: it runs with elevated rights outside this project".into(),
                );
            }
            if words.iter().any(|word| NETWORK.contains(word)) {
                if config.offline {
                    return Deny("The app is offline; network commands are disabled".into());
                }
                if !config.network {
                    return Deny(
                        "Enable network commands in permissions before running this command".into(),
                    );
                }
            }
            if destructive_git(command) {
                return Ask(
                    "Destructive Git command: uncommitted work or history can be lost".into(),
                );
            }
            if config.shell_asks() {
                if tool == "background_start" {
                    return Ask("Start a project background process. It continues independently after this coding task, including cancellation, until stopped or the application closes. Its shell effects are not undone by rewind.".into());
                }
                return Ask("Run a shell command as your user; it can affect files and services beyond this project".into());
            }
            if config.require_approval_for_dangerous
                && words.iter().any(|word| DANGEROUS.contains(word))
            {
                return Ask("Potentially destructive shell command".into());
            }
            // The parsed command: a step that deletes, reaches outside the
            // project, runs downloaded code or needs admin rights still asks,
            // and so does a command with a syntax ShadowCode cannot read.
            // Relative paths count as inside the project; this only adds
            // prompts.
            let root = std::path::Path::new("/.shadowcode-project");
            let cwd = args["cwd"].as_str().map(|cwd| root.join(cwd));
            let assessment = crate::approvals::assess::command(command, root, cwd.as_deref());
            let risky = assessment.risk >= crate::approvals::assess::Risk::Destructive
                || assessment.known_outside
                || !assessment.complete;
            if config.require_approval_for_dangerous && risky {
                Ask(format!(
                    "{} ({})",
                    assessment.explanation.trim_end_matches('.'),
                    assessment.risk.label()
                ))
            } else {
                Allow
            }
        }
        _ => Deny(format!("Unknown or unregistered tool: {tool}")),
    }
}

/// What ShadowCode can and cannot enforce for each vendor CLI, keyed by vendor
/// id. Returned with GET /api/config as `permissions.vendor_notes`.
pub fn vendor_notes(config: &Config) -> Value {
    let mode = match config.permissions.mode {
        crate::config::PermissionMode::Ask => "Ask before actions",
        crate::config::PermissionMode::AllowEdits => "Allow project edits",
    };
    json!({
        "native": format!("{mode}: enforced by ShadowCode for every tool call. Shell commands always ask; sudo/su/pkexec/doas are blocked; network commands follow the network setting; read-only tasks cannot change files."),
        "codex": "Codex runs in its own sandbox: workspace-write for tasks (read-only for Plan/Review) with its approval requests shown here. ShadowCode cannot enforce 'Ask before actions' for edits inside that sandbox; Codex decides which actions need approval.",
        "claude": "Claude Code permission prompts are routed to ShadowCode and you answer them here; Plan/Review uses Claude's plan mode. Claude's own settings files can pre-approve tools that ShadowCode never sees.",
        "cursor": "Cursor sends ACP permission requests that ShadowCode shows for approval; Plan/Review uses Cursor's plan mode when the agent offers it. Actions Cursor does not ask about are outside ShadowCode's control.",
        "grok": "Grok sends ACP permission requests that ShadowCode shows for approval. Grok advertises no read-only mode, so Plan/Review is not enforced by the runtime.",
        "antigravity": "Antigravity runs through Google's ACP agent server and asks ShadowCode before running commands or editing files, as Cursor and Grok do.",
        "network": "Network limits, the shell sandbox and the sudo block apply to ShadowCode's own tools only. Vendor CLIs use their own network access and sandbox; their file changes are still checkpointed for Rewind."
    })
}

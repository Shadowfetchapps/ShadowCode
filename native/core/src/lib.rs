//! Native application engine shared by the desktop, CLI, and MCP transports.
#[cfg(unix)]
pub mod acp_server;
pub mod agents;
pub mod allowance;
pub mod approvals;
pub mod automations;
pub mod autonomy;
pub mod background;
pub mod checkpoint;
#[cfg(unix)]
pub mod cli;
pub mod cli_agent;
pub mod code_intel;
pub mod compaction;
pub mod compare;
pub mod config;
pub mod context;
#[cfg(unix)]
pub mod control;
pub mod effort;
pub mod engine;
pub mod events;
pub mod gguf;
pub mod git_guard;
pub mod guardian;
pub mod hooks;
pub mod instructions;
pub mod intelligence;
pub mod issues;
#[cfg(unix)]
pub mod lifecycle;
pub mod local_downloads;
pub mod local_engine;
pub mod local_runtime;
pub mod local_templates;
pub mod lsp;
#[cfg(unix)]
pub mod mcp;
pub mod memory;
pub mod mentions;
pub mod model_registry;
pub mod models;
pub mod notify;
pub mod ollama_store;
pub mod openrouter;
pub mod page_context;
pub mod parallel;
pub mod patch;
pub mod paths;
pub mod permissions;
#[cfg(unix)]
pub mod plugins;
#[cfg(target_os = "linux")]
pub mod preview;
pub mod process;
pub mod project;
pub mod prompt_cache;
pub mod redaction;
#[cfg(unix)]
pub mod remote;
pub mod retry;
pub mod review;
pub mod routing;
pub mod runtime;
pub mod sandbox;
pub mod service;
#[cfg(unix)]
pub mod sqlite;
pub mod steering;
pub mod store;
pub mod subagents;
pub mod symbol_index;
pub mod system_info;
pub mod terminal;
pub mod textdiff;
pub mod timing;
pub mod tools;
pub mod updates;
pub mod usage;
pub mod vision;
pub mod voice;
pub mod web;
pub mod workflows;
pub mod workspace;
pub mod worktree_tasks;
pub mod worktrees;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub mod verification;

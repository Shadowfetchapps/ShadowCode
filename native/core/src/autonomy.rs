//! 0.21 autonomy helpers. These classify existing tools, budgets, and
//! recovery — they do not replace context compaction, checkpoints, or
//! permissions.
use crate::{context, permissions, tools};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    ReadOnly,
    WorkspaceMutation,
    Process,
    Network,
    External,
    Privileged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayClass {
    SafeToReplay,
    ReEvaluate,
    RequiresConfirmation,
    NeverAutoReplay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Success,
    Partial,
    Failure,
    Cancelled,
    TimedOut,
    Denied,
    Truncated,
    NotFound,
    Conflict,
    Retryable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunawayAction {
    Continue,
    Warn,
    Replan,
    Pause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimLevel {
    ModelClaim,
    Observed,
    Verified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutonomyCaps {
    pub max_steps: usize,
    pub max_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GitSafety {
    pub dirty: bool,
    pub staged: bool,
    pub untracked: bool,
    pub detached: bool,
    pub conflicted: bool,
    pub rebase_or_merge: bool,
    pub unusual_names: Vec<String>,
    pub binary_or_huge: bool,
    pub risk: &'static str,
    pub checkpoint_required: bool,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CapabilityProfile {
    pub provider: String,
    pub context_window: usize,
    pub tools: bool,
    pub structured_output: bool,
    pub reasoning: bool,
    pub multimodal: bool,
    pub streaming: bool,
    pub parallel_tools: bool,
    pub quirks: Vec<&'static str>,
}

pub fn tool_class(name: &str) -> ToolClass {
    if permissions::read_only(name) {
        return ToolClass::ReadOnly;
    }
    match name {
        "write_file" | "edit_file" | "apply_patch" | "create_directory" | "move_file"
        | "delete_file" => ToolClass::WorkspaceMutation,
        "exec" | "background_start" | "background_stop" => ToolClass::Process,
        "mcp_call" | "mcp_tools" => ToolClass::External,
        "git_reset" | "git_clean" => ToolClass::Privileged,
        name if name.starts_with("git_") => ToolClass::WorkspaceMutation,
        _ => ToolClass::External,
    }
}

/// Crash recovery must never assume a mutation finished. Reads may be
/// repeated; shell and Git history changes need a human.
pub fn replay_class(name: &str) -> ReplayClass {
    match name {
        "system_info" | "list_files" | "read_file" | "search_files" | "search_text"
        | "search_symbol" | "workspace_symbols" | "goto_definition" | "find_references"
        | "get_diagnostics" | "get_type_signature" | "repo_map" | "search_code"
        | "mcp_sqlite_tables" | "mcp_sqlite_query" | "background_list" | "background_output"
        | "git_status" | "git_diff" | "git_log" | "update_plan" | "update_todos" => {
            ReplayClass::SafeToReplay
        }
        "git_branch" => ReplayClass::ReEvaluate,
        "exec" | "background_start" | "background_stop" | "mcp_call" | "git_commit"
        | "git_checkout" | "git_add" => ReplayClass::RequiresConfirmation,
        "write_file" | "edit_file" | "apply_patch" | "create_directory" | "move_file"
        | "delete_file" | "git_reset" | "git_clean" => ReplayClass::NeverAutoReplay,
        _ => ReplayClass::RequiresConfirmation,
    }
}

pub fn tool_status(success: bool, output: &Value, error: &str) -> ToolStatus {
    let timed_out = output["timed_out"] == true
        || output["timeout"] == true
        || error.contains("timed out")
        || error.contains("timed_out");
    let cancelled = output["cancelled"] == true || error.contains("cancelled");
    let denied = error.contains("denied")
        || error.contains("Permission")
        || error.contains("unavailable in read-only");
    let not_found = error.contains("not found")
        || error.contains("does not exist")
        || output["error"]
            .as_str()
            .is_some_and(|e| e.contains("not found"));
    let conflict = error.contains("changed after")
        || error.contains("stale")
        || error.contains("expected_hash")
        || error.contains("Checkpoint mismatch");
    let truncated = output["truncated"] == true;
    if cancelled {
        return ToolStatus::Cancelled;
    }
    if timed_out {
        return ToolStatus::TimedOut;
    }
    if denied {
        return ToolStatus::Denied;
    }
    if conflict {
        return ToolStatus::Conflict;
    }
    if not_found {
        return ToolStatus::NotFound;
    }
    if error.contains("connect") || error.contains("429") || error.contains("temporar") {
        return ToolStatus::Retryable;
    }
    if success && truncated {
        return ToolStatus::Truncated;
    }
    if success {
        return ToolStatus::Success;
    }
    if truncated || output.get("exit_code").is_some() {
        return ToolStatus::Partial;
    }
    ToolStatus::Failure
}

pub fn caps_for(profile: &str) -> Option<AutonomyCaps> {
    match profile {
        "conservative" => Some(AutonomyCaps {
            max_steps: 16,
            max_tokens: 64_000,
        }),
        "normal" => Some(AutonomyCaps {
            max_steps: 64,
            max_tokens: 1_000_000,
        }),
        "extended" => Some(AutonomyCaps {
            max_steps: 200,
            max_tokens: 4_000_000,
        }),
        "unlimited" | "custom" => None,
        _ => None,
    }
}

/// Named profiles never raise the configured caps. Unlimited/custom use
/// the user's configured limits as-is.
pub fn effective_caps(
    profile: &str,
    configured_steps: usize,
    configured_tokens: u64,
) -> AutonomyCaps {
    match caps_for(profile) {
        Some(named) => AutonomyCaps {
            max_steps: named.max_steps.min(configured_steps),
            max_tokens: named.max_tokens.min(configured_tokens),
        },
        None => AutonomyCaps {
            max_steps: configured_steps,
            max_tokens: configured_tokens,
        },
    }
}

pub fn budget_status(used_steps: usize, used_tokens: u64, caps: AutonomyCaps) -> Value {
    let step_ratio = used_steps as f64 / caps.max_steps.max(1) as f64;
    let token_ratio = used_tokens as f64 / caps.max_tokens.max(1) as f64;
    let ratio = step_ratio.max(token_ratio);
    let approaching = ratio >= 0.75;
    // Steps are enforced by the engine loop. Treat only the token cap as
    // exhausted here so the final allowed step can still run its tools.
    let exhausted = used_tokens >= caps.max_tokens;
    json!({
        "used_steps": used_steps,
        "used_tokens": used_tokens,
        "max_steps": caps.max_steps,
        "max_tokens": caps.max_tokens,
        "approaching": approaching,
        "exhausted": exhausted,
        "ratio": (ratio * 1000.0).round() / 1000.0,
        "silent_kill": false
    })
}

/// Progressive loop policy. Count 3 warns, 4 asks for a replan, 5 pauses.
/// Legitimate iteration with changing arguments is a different key.
pub fn runaway_action(repeats: usize) -> RunawayAction {
    match repeats {
        0..=2 => RunawayAction::Continue,
        3 => RunawayAction::Warn,
        4 => RunawayAction::Replan,
        _ => RunawayAction::Pause,
    }
}

/// Collapse Gemma/Ollama thinking-channel markup so the transcript keeps the
/// answer and hides private scratch (`thought`, `<channel|>`, analysis tags).
pub fn public_assistant_text(text: &str) -> String {
    let mut out = text.to_owned();
    // XML-ish channel / thinking wrappers and bare close tags.
    let tag = regex::Regex::new(
        r"(?is)</?(?:\|)?(?:channel|think(?:ing)?|redacted[_-]?reasoning|analysis|reasoning)(?:\|)?[^>\n]*>",
    )
    .expect("thinking tag regex");
    out = tag.replace_all(&out, "").into_owned();
    // Standalone channel markers that never formed a closed tag.
    let bare = regex::Regex::new(r"(?i)<\|?channel\|?>").expect("bare channel regex");
    out = bare.replace_all(&out, "").into_owned();
    // Line-leading thinking labels dumped into content by local models.
    let labeled =
        regex::Regex::new(r"(?im)^[ \t]*(?:thought|thinking|analysis|reasoning)\b[^\n]*\n?")
            .expect("thinking label regex");
    out = labeled.replace_all(&out, "").into_owned();
    // Inline "thought …" prefixes before the real sentence.
    let inline = regex::Regex::new(r"(?i)\bthought\b[ \t]*").expect("inline thought regex");
    out = inline.replace_all(&out, "").into_owned();
    let blank = regex::Regex::new(r"\n{3,}").expect("blank collapse regex");
    blank.replace_all(out.trim(), "\n\n").into_owned()
}

fn normalize_loop_unit(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Strongest repeated paragraph/sentence/phrase in assistant text.
/// Returns `(normalized_unit, occurrences)` when a non-trivial unit repeats.
pub fn text_loop_stats(text: &str) -> Option<(String, usize)> {
    let mut best: Option<(String, usize)> = None;
    let mut consider = |unit: String, count: usize| {
        if unit.chars().count() < 12 || count < 3 {
            return;
        }
        if best.as_ref().is_none_or(|(_, previous)| count > *previous) {
            best = Some((unit, count));
        }
    };

    let lines: Vec<String> = text
        .lines()
        .map(normalize_loop_unit)
        .filter(|line| !line.is_empty())
        .collect();
    let mut index = 0;
    while index < lines.len() {
        let mut end = index + 1;
        while end < lines.len() && lines[end] == lines[index] {
            end += 1;
        }
        consider(lines[index].clone(), end - index);
        index = end;
    }

    let mut frequencies: BTreeMap<String, usize> = BTreeMap::new();
    for paragraph in text.split("\n\n") {
        let key = normalize_loop_unit(paragraph);
        if key.chars().count() >= 12 {
            *frequencies.entry(key).or_insert(0) += 1;
        }
    }
    for (unit, count) in frequencies {
        consider(unit, count);
    }

    let collapsed = normalize_loop_unit(text);
    if collapsed.chars().count() >= 60 {
        let chars: Vec<char> = collapsed.chars().collect();
        let max_len = chars.len().min(96);
        for len in (16..=max_len).rev() {
            if chars.len() < len * 3 {
                continue;
            }
            let candidate: String = chars[..len].iter().collect();
            if candidate.split_whitespace().count() < 3 {
                continue;
            }
            let mut count = 0usize;
            let mut cursor = 0usize;
            while cursor + len <= chars.len() {
                let slice: String = chars[cursor..cursor + len].iter().collect();
                if slice == candidate {
                    count += 1;
                    cursor += len;
                    while cursor < chars.len() && chars[cursor].is_whitespace() {
                        cursor += 1;
                    }
                } else {
                    break;
                }
            }
            if count >= 3 {
                consider(candidate, count);
                break;
            }
        }
    }

    best
}

/// First-person claims that a shell/desktop command is being run in prose
/// without a structured tool call. Advice directed at the user is ignored.
pub fn claims_command_execution(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let first_person = regex::Regex::new(
        r"(?i)\b(?:i(?:'m| am)?(?:\s+\w+){0,3}\s+(?:going\s+to|about\s+to)|i(?:'ll| will)|let\s+me|now\s+i(?:'ll| will)?)\s+(?:run|execute|do|set|call|invoke|issue)\b",
    )
    .expect("prose claim regex");
    if first_person.is_match(&lower) {
        return true;
    }
    let running = regex::Regex::new(
        r"(?i)\b(?:running|executing|issuing)\s+(?:`[^`]+`|[a-z0-9._/-]+(?:\s+-[a-z0-9-]+)*)",
    )
    .expect("running claim regex");
    if running.is_match(&lower) {
        return true;
    }
    // Bare "Actually, I'll do: cmd …" lines seen from abliterated local models.
    let do_colon =
        regex::Regex::new(r"(?i)\bi(?:'ll| will)\s+do\s*:").expect("do-colon claim regex");
    do_colon.is_match(&lower)
}

pub fn capability_profile(provider: &str, context_limit: usize) -> CapabilityProfile {
    capability_profile_for(provider, "", context_limit)
}

pub fn capability_profile_for(
    provider: &str,
    model: &str,
    context_limit: usize,
) -> CapabilityProfile {
    let local = matches!(
        provider,
        "ollama" | "local" | "llamacpp" | "lmstudio" | "vllm" | "mock"
    );
    CapabilityProfile {
        provider: provider.into(),
        context_window: context_limit,
        tools: provider != "mock",
        structured_output: !local,
        reasoning: matches!(provider, "openai" | "openrouter" | "anthropic"),
        multimodal: crate::vision::model_supports_vision(provider, model)
            || (model.is_empty() && matches!(provider, "openai" | "openrouter")),
        streaming: true,
        parallel_tools: !matches!(provider, "ollama" | "llamacpp"),
        quirks: match provider {
            "ollama" => vec![
                "unindexed tool frames start a new call unless an id matches",
                "thinking is not forwarded as assistant text",
            ],
            "local" | "lmstudio" | "llamacpp" => vec![
                "later tool deltas often omit index and repeat id/name",
                "argument fragments may arrive as a one-element array",
            ],
            "openai" | "openrouter" => vec!["indexed tool streams are the default"],
            _ => vec!["treat missing usage as estimated"],
        },
    }
}

/// Complete tool descriptions for models with room for them: 32K+ context,
/// or 16K+ for hosted models (which follow longer instructions well). Small
/// local models keep the short catalog so their response window survives.
pub fn description_tier(profile: &CapabilityProfile) -> crate::tools::DescriptionTier {
    let local = matches!(
        profile.provider.as_str(),
        "ollama" | "local" | "llamacpp" | "lmstudio" | "vllm" | "mock"
    );
    if profile.context_window >= 32_768 || (!local && profile.context_window >= 16_384) {
        crate::tools::DescriptionTier::Full
    } else {
        crate::tools::DescriptionTier::Short
    }
}

pub fn account(
    messages: &[Value],
    schemas: &[Value],
    context_limit: usize,
) -> Result<Value, anyhow::Error> {
    let reserved = context::response_budget(messages, schemas, context_limit).unwrap_or(256);
    let mut layers = BTreeMap::from([
        ("system", 0usize),
        ("live", 0),
        ("working", 0),
        ("project", 0),
        ("artifact", 0),
        ("historical", 0),
        ("tools", context::estimate_tokens(&json!(schemas))),
        ("reserved_output", reserved),
    ]);
    let last_user = messages.iter().rposition(|m| m["role"] == "user");
    for (index, message) in messages.iter().enumerate() {
        let tokens = context::estimate_tokens(message);
        let role = message["role"].as_str().unwrap_or("");
        let content = message["content"].as_str().unwrap_or("");
        if message["_shadow_compaction"] == true {
            *layers.get_mut("historical").unwrap() += tokens;
            continue;
        }
        if role == "system" {
            if content.contains("Project guidance from") {
                *layers.get_mut("project").unwrap() += tokens;
            } else {
                *layers.get_mut("system").unwrap() += tokens;
            }
            continue;
        }
        if last_user == Some(index) || last_user.is_some_and(|i| index > i) {
            *layers.get_mut("live").unwrap() += tokens;
            continue;
        }
        if role == "tool" && content.len() > 2000 {
            *layers.get_mut("artifact").unwrap() += tokens;
        } else if role == "tool" || message.get("tool_calls").is_some() {
            *layers.get_mut("working").unwrap() += tokens;
        } else {
            *layers.get_mut("historical").unwrap() += tokens;
        }
    }
    let used: usize = layers.values().sum();
    Ok(json!({
        "layers": layers,
        "used_estimated_tokens": used,
        "limit": context_limit,
        "remaining": context_limit.saturating_sub(used),
        "fits": used + 256 <= context_limit,
        "method": "deterministic_char_div3"
    }))
}

/// Extract what compaction must not silently forget. This is a keep-list
/// for the inspectable note, not a model-written summary.
pub fn preserve(messages: &[Value]) -> Value {
    let mut intent = Vec::new();
    let mut constraints = Vec::new();
    let mut decisions = Vec::new();
    let mut unresolved = Vec::new();
    let mut completed = Vec::new();
    let mut failed = Vec::new();
    let mut files = BTreeSet::new();
    let mut verification = Vec::new();
    let mut plan = Value::Null;
    let mut security = Vec::new();
    for message in messages {
        let role = message["role"].as_str().unwrap_or("");
        let content = message["content"].as_str().unwrap_or("");
        if role == "user" && intent.len() < 4 {
            intent.push(tools::truncate(content, 280).to_owned());
        }
        if role == "user" && looks_like_constraint(content) && constraints.len() < 8 {
            constraints.push(tools::truncate(content, 240).to_owned());
        }
        if looks_like_decision(content) && decisions.len() < 8 {
            decisions.push(tools::truncate(content, 240).to_owned());
        }
        if role == "assistant" {
            if let Some(calls) = message["tool_calls"].as_array() {
                for call in calls {
                    if let Ok(args) = serde_json::from_str::<Value>(
                        call["function"]["arguments"].as_str().unwrap_or("{}"),
                    ) {
                        collect_paths(&args, &mut files);
                    }
                }
            }
        }
        if role == "tool" {
            if let Ok(body) = serde_json::from_str::<Value>(content) {
                collect_paths(&body, &mut files);
                let name = message["name"].as_str().unwrap_or("");
                if body["success"] == false || body["ok"] == false {
                    let error_text =
                        tools::truncate(body["error"].as_str().unwrap_or(content), 160);
                    if failed.len() < 8 {
                        failed.push(format!("{name}: {error_text}"));
                    }
                    if unresolved.len() < 8 {
                        unresolved.push(name.to_owned());
                    }
                    if error_text.to_ascii_lowercase().contains("denied") && security.len() < 6 {
                        security.push(error_text.to_owned());
                    }
                } else if matches!(
                    name,
                    "write_file" | "edit_file" | "apply_patch" | "create_directory"
                ) && completed.len() < 8
                {
                    completed.push(name.to_owned());
                }
                if name == "exec" {
                    verification.push(json!({
                        "command": body["command"],
                        "success": body["success"],
                        "exit_code": body["exit_code"],
                        "timed_out": body["timed_out"]
                    }));
                }
            } else if content.contains("denied") && security.len() < 6 {
                security.push(tools::truncate(content, 160).to_owned());
            }
        }
        if content.contains("\"steps\"") {
            if let Ok(body) = serde_json::from_str::<Value>(content) {
                if body.get("steps").is_some() {
                    plan = body;
                }
            }
        }
    }
    json!({
        "intent": intent,
        "constraints": constraints,
        "decisions": decisions,
        "unresolved": unresolved,
        "completed": completed,
        "failed_approaches": failed,
        "file_locations": files.into_iter().take(24).collect::<Vec<_>>(),
        "verification": verification,
        "plan": plan,
        "security": security,
        "method": "deterministic_keep_list"
    })
}

fn looks_like_constraint(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("must not")
        || lower.contains("do not")
        || lower.contains("never ")
        || lower.contains("required:")
        || lower.contains("constraint")
}

fn looks_like_decision(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("decision:")
        || lower.contains("decided to")
        || lower.contains("we will use")
        || lower.contains("chose to")
}

fn collect_paths(value: &Value, files: &mut BTreeSet<String>) {
    for key in ["path", "src", "dest"] {
        if let Some(path) = value[key]
            .as_str()
            .filter(|p| !p.is_empty() && p.len() <= 512)
        {
            files.insert(path.to_owned());
        }
    }
    if let Some(paths) = value["paths"].as_array() {
        for path in paths.iter().filter_map(|p| p.as_str()).take(16) {
            if path.len() <= 512 {
                files.insert(path.to_owned());
            }
        }
    }
}

pub fn classify_verification(model_text: &str, commands: &[Value], inspected: bool) -> Value {
    crate::verification::classify(model_text, commands, inspected)
}

/// When the user goal is a bug/fix, ask for a failing test first. Not applied
/// to every prompt — only when the task text indicates a bug fix.
pub fn bugfix_policy(task: &str) -> Option<&'static str> {
    // Whole-word matching: "prefix", "fixture" and "debug" are not bug reports.
    let is_hint = |word: &str| {
        matches!(
            word,
            "bug"
                | "bugs"
                | "bugfix"
                | "fix"
                | "fixes"
                | "fixed"
                | "fixing"
                | "hotfix"
                | "broken"
                | "breaks"
                | "fail"
                | "fails"
                | "failed"
                | "failing"
                | "failure"
                | "failures"
                | "crash"
                | "crashes"
                | "crashed"
                | "crashing"
                | "incorrect"
                | "incorrectly"
                | "defect"
                | "defects"
        ) || word.starts_with("regress")
    };
    let mut words = task
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase);
    if words.any(|word| is_hint(&word)) {
        Some(
            "Bug-fix policy: reproduce with a failing test or clear reproduction command before claiming a fix. Do not present the task as verified until that failing case and a subsequent passing check are observed in this task.",
        )
    } else {
        None
    }
}

pub(crate) fn looks_like_success_claim(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("tests passed")
        || lower.contains("all tests pass")
        || lower.contains("verified")
        || lower.contains("build succeeded")
        || lower.contains("correctly implemented")
}

pub fn parse_git_status(branch_line: &str, entries: &str) -> GitSafety {
    let detached = branch_line.contains("detached") || branch_line.starts_with("## HEAD");
    let rebase_or_merge = branch_line.contains("rebasing")
        || branch_line.contains("merging")
        || entries
            .lines()
            .any(|line| line.starts_with("u ") || line.contains("UU "));
    let mut dirty = false;
    let mut staged = false;
    let mut untracked = false;
    let mut conflicted = rebase_or_merge;
    let mut unusual_names = Vec::new();
    let mut binary_or_huge = false;
    for line in entries.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with("?? ") {
            untracked = true;
        } else if line.starts_with("u ") || line.contains("UU ") || line.contains("AA ") {
            conflicted = true;
        } else {
            dirty = true;
            if line.len() >= 2 && !line.starts_with(' ') && &line[..1] != "?" {
                staged = staged || !line.starts_with(' ');
            }
            if line
                .as_bytes()
                .first()
                .is_some_and(|c| *c != b'?' && *c != b' ')
            {
                staged = true;
            }
        }
        let name = line.split_once(' ').map(|(_, rest)| rest).unwrap_or("");
        let file = name.rsplit_once(' ').map(|(_, n)| n).unwrap_or(name);
        if file.starts_with('-')
            || file.contains('\0')
            || file.contains('\n')
            || file.contains("..")
        {
            unusual_names.push(tools::truncate(file, 80).to_owned());
        }
        if file.ends_with(".bin") || file.ends_with(".wasm") || file.ends_with(".so") {
            binary_or_huge = true;
        }
    }
    let checkpoint_required = dirty || staged || conflicted || rebase_or_merge;
    let risk = if conflicted || rebase_or_merge {
        "conflict"
    } else if detached {
        "detached"
    } else if checkpoint_required {
        "dirty"
    } else {
        "clean"
    };
    GitSafety {
        dirty,
        staged,
        untracked,
        detached,
        conflicted,
        rebase_or_merge,
        unusual_names,
        binary_or_huge,
        risk,
        checkpoint_required,
        note: match risk {
            "conflict" => "Do not auto-commit, reset, or clean. Conflicts need a human.".into(),
            "detached" => {
                "HEAD is detached. Do not reset or create commits without confirmation.".into()
            }
            "dirty" => "Take a safety checkpoint before destructive Git or shell work.".into(),
            _ => "Working tree is clean enough for ordinary read-only Git tools.".into(),
        },
    }
}

pub fn worktree_recovery_advice(reason: &str) -> Value {
    let (safe, action) = if reason.contains("moved") || reason.contains("path") {
        (false, "Refuse auto-recovery. Re-open the original real path; do not guess a relocated checkout.")
    } else if reason.contains("metadata") || reason.contains("lost") || reason.contains("index") {
        (false, "Refuse auto-recovery. Restore Git administrative files from the recorded worktree identity.")
    } else if reason.contains("locked") {
        (false, "Another Git process holds the worktree. Wait or inspect the lock; do not delete it blindly.")
    } else {
        (
            false,
            "Auto-recovery is unsafe. Surface the recorded paths and require review.",
        )
    };
    json!({"auto_recover":safe,"action":action,"guess_paths":false})
}

pub fn catalog() -> Value {
    let names = [
        "list_files",
        "read_file",
        "search_files",
        "search_text",
        "search_symbol",
        "workspace_symbols",
        "goto_definition",
        "find_references",
        "get_diagnostics",
        "repo_map",
        "search_code",
        "mcp_sqlite_tables",
        "mcp_sqlite_query",
        "background_start",
        "background_list",
        "background_output",
        "background_stop",
        "write_file",
        "edit_file",
        "apply_patch",
        "create_directory",
        "move_file",
        "delete_file",
        "exec",
        "git_status",
        "git_diff",
        "git_log",
        "git_branch",
        "git_checkout",
        "git_add",
        "git_commit",
        "git_reset",
        "git_clean",
        "update_plan",
        "mcp_tools",
        "mcp_call",
    ];
    json!(names
        .iter()
        .map(|name| json!({
            "name": name,
            "class": tool_class(name),
            "replay": replay_class(name),
            "read_only": permissions::read_only(name)
        }))
        .collect::<Vec<_>>())
}

/// Shell policy study helpers. Lexical word lists are not a sandbox.
pub fn shell_policy_limits() -> Value {
    json!({
        "sandbox": false,
        "method": "lexical_word_list",
        "false_negatives": [
            "quoted sudo via env/alias",
            "interpreter -c with destructive payload",
            "curl via python/node",
            "rm via find -exec or xargs"
        ],
        "false_positives": [
            "comments mentioning curl",
            "paths named rm-backup",
            "npm as a local project script name"
        ],
        "do_not_claim": "OS sandbox or complete command safety"
    })
}

/// Suggest the narrowest relevant test command when the user/task asked to verify.
/// Returns None when verification was not requested.
pub fn narrow_verify_command(task: &str, workspace: &std::path::Path) -> Option<String> {
    let lower = task.to_ascii_lowercase();
    let asks = [
        "verify",
        "run the test",
        "run tests",
        "run the tests",
        "cargo test",
        "npm test",
        "pytest",
        "make sure tests",
        "check that",
    ];
    if !asks.iter().any(|a| lower.contains(a)) && bugfix_policy(task).is_none() {
        // Only auto-suggest for bug/fix when combined with verify language elsewhere;
        // bugfix_policy alone asks for failing test first, not necessarily to run suite.
        return None;
    }
    if !asks.iter().any(|a| lower.contains(a)) {
        return None;
    }
    if workspace.join("Cargo.toml").is_file() {
        let words: Vec<_> = task.split_whitespace().collect();
        for pair in words.windows(2) {
            if pair[0] == "-p"
                && !pair[1].is_empty()
                && pair[1]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            {
                return Some(format!("cargo test -p {}", pair[1]));
            }
        }
        return Some("cargo test".into());
    }
    if workspace.join("package.json").is_file() {
        return Some("npm test".into());
    }
    if workspace.join("pyproject.toml").is_file() || workspace.join("pytest.ini").is_file() {
        return Some("pytest".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_auto_replays_mutations_or_destructive_git() {
        assert_eq!(replay_class("read_file"), ReplayClass::SafeToReplay);
        assert_eq!(replay_class("exec"), ReplayClass::RequiresConfirmation);
        assert_eq!(replay_class("write_file"), ReplayClass::NeverAutoReplay);
        assert_eq!(replay_class("git_reset"), ReplayClass::NeverAutoReplay);
    }

    #[test]
    fn runaway_is_progressive() {
        assert_eq!(runaway_action(1), RunawayAction::Continue);
        assert_eq!(runaway_action(3), RunawayAction::Warn);
        assert_eq!(runaway_action(4), RunawayAction::Replan);
        assert_eq!(runaway_action(5), RunawayAction::Pause);
    }

    #[test]
    fn thinking_channels_are_stripped_but_answers_remain() {
        let raw = "thought <channel|>I'll set all three monitors to green.\n</channel>\nAll three displays are solid green.";
        let visible = public_assistant_text(raw);
        assert!(!visible.to_ascii_lowercase().contains("thought"));
        assert!(!visible.contains("<channel"));
        assert!(visible.contains("All three displays are solid green."));
        assert_eq!(
            public_assistant_text("Fixed the typo in main.rs."),
            "Fixed the typo in main.rs."
        );
    }

    #[test]
    fn repeated_paragraphs_trip_text_loop_stats() {
        let spam = "Actually, I'll do: xpaper -bg\n".repeat(8);
        let (unit, count) = text_loop_stats(&spam).expect("loop");
        assert!(unit.contains("xpaper"));
        assert!(count >= 5, "{count}");
        assert!(text_loop_stats("Short unique answer about the edit.").is_none());
        let normal =
            "First I read the file.\n\nThen I patched the helper.\n\nFinally I ran cargo test.";
        assert!(text_loop_stats(normal).is_none());
    }

    #[test]
    fn prose_command_claims_are_detected() {
        assert!(claims_command_execution(
            "Actually, I'll do: xpaper -bg green on each monitor"
        ));
        assert!(claims_command_execution(
            "I'll run xset root solid green now."
        ));
        assert!(claims_command_execution(
            "Running `feh --bg-fill green.png`."
        ));
        assert!(!claims_command_execution(
            "You can run cargo test after reviewing the diff."
        ));
        assert!(!claims_command_execution(
            "Updated README with install steps."
        ));
    }

    #[test]
    fn last_allowed_step_is_not_a_token_exhaustion() {
        let status = budget_status(
            1,
            30,
            AutonomyCaps {
                max_steps: 1,
                max_tokens: 1_000_000,
            },
        );
        assert_eq!(status["exhausted"], false);
        assert_eq!(status["approaching"], true);
    }

    #[test]
    fn profiles_never_raise_configured_caps() {
        let caps = effective_caps("extended", 64, 1_000_000);
        assert_eq!(caps.max_steps, 64);
        assert_eq!(caps.max_tokens, 1_000_000);
        let unlimited = effective_caps("unlimited", 64, 1_000_000);
        assert_eq!(unlimited.max_steps, 64);
    }

    fn check_receipt(command: &str, success: bool) -> Value {
        json!({"schema_version":1,"task_id":"task","attempt_id":"attempt","tool_call_id":"call","check_id":crate::workspace::hash(command.as_bytes()),
            "workspace":"/project","cwd":"/project","command":command,"kind":"configured_check",
            "state":if success {"passed"}else{"failed"},"provenance":"locally_observed","scope":"configured check",
            "started_at":1.0,"finished_at":2.0,"exit_code":if success{0}else{1},"termination_reason":"exited",
            "workspace_fingerprint":"content-hash","output_ref":"tool.completed:call","success":success,"timed_out":false})
    }

    #[test]
    fn model_prose_is_not_verification() {
        let report = classify_verification("All tests passed.", &[], false);
        assert_eq!(report["claim"], "model_claim");
        assert_eq!(report["verified"], false);
        assert_eq!(report["unverified_claim"], true);
        let observed = classify_verification(
            "Looks good",
            &[json!({"command":"ls","success":true})],
            true,
        );
        assert_eq!(observed["claim"], "observed");
        let verified =
            classify_verification("Done", &[check_receipt("cargo test --offline", true)], true);
        assert_eq!(verified["claim"], "verified");
        assert_eq!(verified["verified"], true);
    }

    #[test]
    fn failing_test_first_then_passing_run_is_verified() {
        // The bug-fix policy asks for a red test before the fix. That earlier,
        // deliberate failure must not block the final passing run.
        let red_green = classify_verification(
            "Fixed",
            &[
                check_receipt("cargo test -p demo", false),
                check_receipt("cargo test -p demo", true),
            ],
            true,
        );
        assert_eq!(red_green["claim"], "verified");
        assert_eq!(red_green["red_green"], true);
        // A failure after the last passing check still blocks verification.
        let regressed = classify_verification(
            "All tests passed",
            &[
                check_receipt("cargo test", true),
                check_receipt("cargo test", false),
            ],
            true,
        );
        assert_eq!(regressed["verified"], false);
        assert_eq!(regressed["unverified_claim"], true);
        // A later failing non-verification command also blocks it.
        let later_failure = classify_verification(
            "Done",
            &[
                check_receipt("npm test", true),
                json!({"command":"node scripts/broken.js","success":false}),
            ],
            true,
        );
        assert_eq!(later_failure["verified"], false);
        // A timed-out final check is not a passing check.
        let timed_out = classify_verification(
            "Done",
            &[{
                let mut receipt = check_receipt("pytest", true);
                receipt["timed_out"] = json!(true);
                receipt
            }],
            true,
        );
        assert_eq!(timed_out["verified"], false);
    }

    #[test]
    fn bugfix_policy_matches_whole_words_only() {
        assert!(bugfix_policy("Fix the crash when saving").is_some());
        assert!(bugfix_policy("Investigate a regression in the parser").is_some());
        assert!(bugfix_policy("This test fails on Windows").is_some());
        assert!(bugfix_policy("Add a prefix to generated filenames").is_none());
        assert!(bugfix_policy("Create a pytest fixture for the database").is_none());
        assert!(bugfix_policy("Add debug logging to the loader").is_none());
    }
}

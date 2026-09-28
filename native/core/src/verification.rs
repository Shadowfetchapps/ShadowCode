//! Verification receipts extend the existing command/task event stream.
//! A successful process is not task acceptance. Only explicit/project check
//! commands contribute a scoped check result, tied to observed file content.
use crate::{
    models::ToolCall,
    tools::{ToolExecutor, ToolResult},
    workspace::Workspace,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc, time::Instant};

/// Native inspection scope records successful observations, not verification.
/// Keep the live loop and durable failure/recovery assessment on one allowlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InspectionScope {
    Workspace,
    Host,
}

pub(crate) const INSPECTION_TOOLS: &[(&str, InspectionScope)] = &[
    ("read_file", InspectionScope::Workspace),
    ("search_text", InspectionScope::Workspace),
    ("search_symbol", InspectionScope::Workspace),
    ("workspace_symbols", InspectionScope::Workspace),
    ("goto_definition", InspectionScope::Workspace),
    ("find_references", InspectionScope::Workspace),
    ("get_diagnostics", InspectionScope::Workspace),
    ("get_type_signature", InspectionScope::Workspace),
    ("repo_map", InspectionScope::Workspace),
    ("search_code", InspectionScope::Workspace),
    ("git_diff", InspectionScope::Workspace),
    ("git_status", InspectionScope::Workspace),
    ("git_log", InspectionScope::Workspace),
    ("mcp_sqlite_tables", InspectionScope::Workspace),
    ("mcp_sqlite_query", InspectionScope::Workspace),
    ("background_list", InspectionScope::Workspace),
    ("background_output", InspectionScope::Workspace),
    ("system_info", InspectionScope::Host),
];

pub(crate) fn inspection_scope(tool: &str) -> Option<InspectionScope> {
    INSPECTION_TOOLS
        .iter()
        .find_map(|(name, scope)| (*name == tool).then_some(*scope))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CheckConfig {
    /// Exact, user-configured checks in the project root. This is not an
    /// approval bypass: execution still uses the normal shell policy.
    pub commands: Vec<String>,
}
impl CheckConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.commands.len() <= 64,
            "At most 64 verification commands"
        );
        for command in &self.commands {
            ensure!(
                !command.trim().is_empty() && command.len() <= 64000 && !command.contains('\0'),
                "Invalid verification command"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    NotRun,
    Running,
    Passed,
    Failed,
    Cancelled,
    Skipped,
    Stale,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Command,
    ConfiguredCheck,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub schema_version: u32,
    pub task_id: String,
    pub attempt_id: String,
    pub tool_call_id: String,
    pub check_id: String,
    pub workspace: String,
    pub cwd: String,
    pub command: String,
    pub kind: Kind,
    pub state: State,
    pub provenance: String,
    pub scope: String,
    pub started_at: f64,
    pub finished_at: f64,
    /// Monotonic duration from the owned process, excluding approval waits
    /// and workspace fingerprinting. Missing when no process result exists.
    #[serde(default)]
    pub process_seconds: Option<f64>,
    pub exit_code: Option<i64>,
    pub termination_reason: String,
    pub workspace_fingerprint: Option<String>,
    pub output_ref: String,
    pub success: bool,
    pub timed_out: bool,
}

/// Bounded content snapshot, including uncommitted content and file modes.
/// Failure/oversize is unknown evidence, never a partial fingerprint. Ignored
/// and generated directories are explicitly outside this receipt's scope.
pub async fn fingerprint(workspace: Arc<Workspace>) -> Option<String> {
    tokio::task::spawn_blocking(move || snapshot(&workspace))
        .await
        .ok()?
        .ok()
}
fn snapshot(workspace: &Workspace) -> Result<String> {
    let started = Instant::now();
    let mut builder = ignore::WalkBuilder::new(&workspace.path);
    builder
        .hidden(false)
        .parents(false)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .filter_entry(|e| {
            e.depth() == 0
                || !matches!(
                    e.file_name().to_str(),
                    Some(
                        ".git"
                            | "node_modules"
                            | "target"
                            | "dist"
                            | "build"
                            | ".venv"
                            | "__pycache__"
                    )
                )
        });
    let mut digest = Sha256::new();
    let mut bytes = 0usize;
    for (count, entry) in builder.build().enumerate() {
        ensure!(
            count < 100_000 && started.elapsed().as_secs() < 5,
            "Fingerprint budget exceeded"
        );
        let entry = entry?;
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            continue;
        }
        let relative = entry.path().strip_prefix(&workspace.path)?;
        let name = relative
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Non-UTF8 path"))?;
        ensure!(
            entry.file_type().is_some_and(|kind| kind.is_file()),
            "Non-regular input cannot be fingerprinted"
        );
        let metadata = std::fs::symlink_metadata(entry.path())?;
        let (content, size) = workspace.inspect(name, crate::workspace::MAX_FILE_BYTES)?;
        ensure!(content.len() as u64 == size, "Fingerprint input too large");
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::MetadataExt;
            metadata.mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = u32::from(metadata.permissions().readonly());
        bytes += content.len();
        ensure!(bytes <= 128_000_000, "Fingerprint byte budget exceeded");
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update(mode.to_le_bytes());
        digest.update((content.len() as u64).to_le_bytes());
        digest.update(&content);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Execute through the existing approval/sandbox/hook path. Tool arguments do
/// not grant check provenance. Explicit check tasks are selected by the user;
/// automatic checks must exactly match project-inspected commands.
pub async fn execute(
    tools: &ToolExecutor,
    call: ToolCall,
    attempt: &str,
    explicit: bool,
) -> Result<ToolResult> {
    let command = call.arguments["command"].as_str().unwrap_or("").to_owned();
    let cwd_arg = call.arguments["cwd"].as_str().unwrap_or(".");
    let cwd = tools
        .workspace
        .relative(cwd_arg)
        .ok()
        .and_then(|p| tools.workspace.path.join(p).canonicalize().ok());
    let check = if explicit {
        true
    } else if cwd.as_ref() == Some(&tools.workspace.path) {
        tools
            .config
            .verification
            .commands
            .iter()
            .any(|configured| configured.trim() == command.trim())
            || crate::project::inspect(tools.workspace.clone())
                .await
                .ok()
                .is_some_and(|map| {
                    map["test_commands"].as_array().is_some_and(|commands| {
                        commands
                            .iter()
                            .any(|row| row["command"].as_str() == Some(command.trim()))
                    })
                })
    } else {
        false
    };
    let before = if check {
        fingerprint(tools.workspace.clone()).await
    } else {
        None
    };
    let started_at = crate::now();
    let mut result = tools.execute(call.clone()).await?;
    let finished_at = crate::now();
    let after = if check {
        fingerprint(tools.workspace.clone()).await
    } else {
        None
    };
    let timed_out = result.output["timed_out"] == true;
    let cancelled = tools.cancel.is_cancelled() || result.output["cancelled"] == true;
    let exit_code = result.output["exit_code"].as_i64();
    let state = if cancelled {
        State::Cancelled
    } else if !result.success || timed_out || exit_code != Some(0) {
        State::Failed
    } else if check && (before.is_none() || after.is_none()) {
        State::Skipped
    } else if check && before != after {
        State::Stale
    } else {
        State::Passed
    };
    let execution = result
        .execution
        .as_ref()
        .context("Executed tool has no durable identity")?;
    let output_ref = format!("event:{}", execution.completed_event_id);
    let receipt = Receipt {
        schema_version: 1, task_id: tools.events.task_id.clone(), attempt_id: attempt.into(),
        tool_call_id: execution.call_id.clone(), workspace: tools.workspace.path.to_string_lossy().into_owned(),
        cwd: cwd.unwrap_or_else(|| tools.workspace.path.clone()).to_string_lossy().into_owned(),
        check_id: crate::workspace::hash(&serde_json::to_vec(&json!([cwd_arg,command])).unwrap_or_default()),
        command, kind: if check { Kind::ConfiguredCheck } else { Kind::Command },
        success: state == State::Passed, state,
        provenance: "locally_observed".into(),
        scope: if check { "Exact configured/project check; non-ignored workspace content excluding generated directories. No assertion about test counts or overall task acceptance." } else { "Process execution only; not verification." }.into(),
        started_at, finished_at, exit_code, timed_out,
        process_seconds: result.output["duration_ms"].as_u64().map(|ms| ms as f64 / 1000.0),
        termination_reason: if cancelled { "cancelled" } else if timed_out { "timeout" } else if exit_code.is_some() { "exited" } else { "not_started_or_failed" }.into(),
        workspace_fingerprint: after, output_ref,
    };
    let mut value = serde_json::to_value(receipt)?;
    crate::redaction::redact_value(&mut value);
    // Only this executor-generated identity is trusted metadata. Commands,
    // paths, outputs and provider-supplied strings remain redacted.
    value["tool_call_id"] = json!(execution.call_id);
    tools.events.emit("verification.receipt", value.clone())?;
    if !result.output.is_object() {
        result.output = json!({});
    }
    result.output["verification_receipt"] = value;
    Ok(result)
}

fn needs_refresh(commands: &[Value]) -> bool {
    commands
        .iter()
        .any(|v| v["kind"] == "configured_check" && v["state"] == "passed")
}

fn apply_fingerprint(commands: &mut [Value], current: Option<&str>) {
    for command in commands {
        if command["kind"] == "configured_check"
            && command["state"] == "passed"
            && (current.is_none() || command["workspace_fingerprint"].as_str() != current)
        {
            command["state"] = json!("stale");
            command["success"] = json!(false);
        }
    }
}

pub async fn refresh(commands: &mut [Value], workspace: Arc<Workspace>) {
    if needs_refresh(commands) {
        let current = fingerprint(workspace).await;
        apply_fingerprint(commands, current.as_deref());
    }
}

/// Revalidate a historical result without rewriting its original receipts.
/// Reopening an application/profile does not make an old pass current.
pub async fn current(engine: &crate::engine::Engine, job: &crate::engine::Job) -> Result<Value> {
    Ok(current_batch(engine, std::slice::from_ref(job))
        .await?
        .remove(0))
}

/// One bounded point-in-time scan per workspace for visible receipt refreshes.
/// Nothing is cached between calls: external writes need no engine event.
pub async fn current_batch(
    engine: &crate::engine::Engine,
    jobs: &[crate::engine::Job],
) -> Result<Vec<Value>> {
    current_batch_using(engine, jobs, |path| async move {
        match Workspace::open(&path) {
            Ok(workspace) => fingerprint(Arc::new(workspace)).await,
            Err(_) => None,
        }
    })
    .await
}

async fn current_batch_using<F, Fut>(
    engine: &crate::engine::Engine,
    jobs: &[crate::engine::Job],
    mut read: F,
) -> Result<Vec<Value>>
where
    F: FnMut(std::path::PathBuf) -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    ensure!(
        !jobs.is_empty() && jobs.len() <= 32,
        "Refresh requires 1 to 32 jobs"
    );
    let mut fingerprints = BTreeMap::new();
    let mut results = Vec::with_capacity(jobs.len());
    for job in jobs {
        let summary = job
            .result
            .as_ref()
            .map(|result| result["verification"].clone())
            .filter(|value| value.is_object())
            .or(engine
                .store()
                .last_task_event(&job.task_id, "verification.summary")?
                .map(|event| event["payload"].clone()))
            .unwrap_or_else(|| json!({"status":"not_run","commands":[]}));
        if summary["status"] == "vendor_owned" {
            results.push(summary);
            continue;
        }
        let mut commands = summary["commands"].as_array().cloned().unwrap_or_default();
        if needs_refresh(&commands) {
            if !fingerprints.contains_key(&job.workspace) {
                fingerprints.insert(job.workspace.clone(), read(job.workspace.clone()).await);
            }
            apply_fingerprint(
                &mut commands,
                fingerprints
                    .get(&job.workspace)
                    .and_then(|value| value.as_deref()),
            );
        }
        results.push(assess_summary(summary, commands, job));
    }
    Ok(results)
}

fn assess_summary(mut summary: Value, commands: Vec<Value>, job: &crate::engine::Job) -> Value {
    let text = if summary["model_claimed_success"] == true {
        "All tests passed"
    } else {
        ""
    };
    let assessed = classify_observations(
        text,
        &commands,
        summary["inspected_workspace"] == true,
        summary["inspected_host"] == true,
    );
    if let (Some(target), Some(fields)) = (summary.as_object_mut(), assessed.as_object()) {
        target.extend(fields.clone());
    }
    if job.status != "completed" {
        summary["verified"] = json!(false);
        if summary["claim"] == "verified" {
            summary["claim"] = json!("observed");
        }
        summary["red_green"] = json!(false);
        summary["status"] = json!(match job.status.as_str() {
            "cancelled" => "cancelled",
            "failed" => "failed",
            _ => "incomplete",
        });
    }
    summary["assessed_at"] = json!(crate::now());
    summary["freshness_scope"] = json!("Point-in-time assessment of current file fingerprints for command receipts. Recorded inspection scope is historical and not revalidated, including legacy summaries without separate host scope. Original task history is unchanged.");
    summary
}

/// The latest receipt for each exact check, for Compare and other consumers.
pub fn latest_checks(commands: &[Value]) -> Vec<Value> {
    let mut latest = BTreeMap::new();
    for value in commands {
        if let Ok(receipt) = serde_json::from_value::<Receipt>(value.clone()) {
            if receipt.schema_version == 1
                && receipt.kind == Kind::ConfiguredCheck
                && receipt.provenance == "locally_observed"
            {
                latest.insert((receipt.cwd, receipt.check_id), value.clone());
            }
        }
    }
    latest.into_values().collect()
}

pub fn classify(text: &str, commands: &[Value], inspected: bool) -> Value {
    classify_observations(text, commands, inspected, false)
}

pub(crate) fn classify_observations(
    text: &str,
    commands: &[Value],
    inspected_workspace: bool,
    inspected_host: bool,
) -> Value {
    // A rerun supersedes only the same exact command in the same directory.
    // An unrelated success can never hide another failed or stale check.
    let mut latest = BTreeMap::new();
    for value in commands {
        if let Ok(receipt) = serde_json::from_value::<Receipt>(value.clone()) {
            if receipt.schema_version == 1
                && receipt.kind == Kind::ConfiguredCheck
                && receipt.provenance == "locally_observed"
            {
                latest.insert((receipt.cwd.clone(), receipt.check_id.clone()), receipt);
            }
        }
    }
    let last_check = commands
        .iter()
        .rposition(|v| v["kind"] == "configured_check");
    let later_failure =
        last_check.is_some_and(|last| commands[last + 1..].iter().any(|v| v["success"] == false));
    let verified = !later_failure
        && !latest.is_empty()
        && latest.values().all(|r| {
            r.state == State::Passed
                && r.success
                && r.exit_code == Some(0)
                && !r.timed_out
                && r.workspace_fingerprint.is_some()
        });
    let execution_failed = commands
        .last()
        .is_some_and(|v| matches!(v["state"].as_str(), Some("failed" | "cancelled")))
        || later_failure;
    let red_green = verified
        && commands
            .iter()
            .any(|v| v["kind"] == "configured_check" && v["state"] == "failed");
    let status = if latest.is_empty() {
        "not_run"
    } else if latest.values().any(|r| r.state == State::Failed) {
        "failed"
    } else if latest.values().any(|r| r.state == State::Cancelled) {
        "cancelled"
    } else if latest.values().any(|r| r.state == State::Stale) {
        "stale"
    } else if verified {
        "passed"
    } else {
        "skipped"
    };
    let claims = crate::autonomy::looks_like_success_claim(text);
    let claim = if verified {
        "verified"
    } else if claims {
        "model_claim"
    } else if inspected_workspace || inspected_host || !commands.is_empty() {
        "observed"
    } else {
        "model_claim"
    };
    json!({"schema_version":1,"status":status,"claim":claim,"verified":verified,
        "model_claimed_success":claims,"inspected_workspace":inspected_workspace,"inspected_host":inspected_host,"commands":commands,"red_green":red_green,
        "unverified_claim":claims && !verified,"execution_failed":execution_failed,
        "note":"Only recorded configured checks count. Passing checks do not establish complete task acceptance; arbitrary commands and model prose are not verification."})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn batch_refresh_shares_only_in_request_fingerprints_and_detects_external_changes() {
        use std::cell::RefCell;
        let root = tempfile::tempdir().unwrap();
        let paths = crate::paths::AppPaths::isolated(&root.path().join("profile")).unwrap();
        let engine = crate::engine::Engine::open(paths).unwrap();
        let mut jobs = Vec::new();
        for project in ["a", "b"] {
            let workspace = root.path().join(project);
            std::fs::create_dir(&workspace).unwrap();
            std::fs::write(workspace.join("source.txt"), "before").unwrap();
            let hash = fingerprint(Arc::new(Workspace::open(&workspace).unwrap()))
                .await
                .unwrap();
            for index in 0..2 {
                let mut command = check("project check", State::Passed);
                command["workspace_fingerprint"] = json!(hash);
                jobs.push(crate::engine::Job {
                    id: format!("{project}-{index}"),
                    task_id: format!("task-{project}-{index}"),
                    workspace: workspace.clone(),
                    status: "completed".into(),
                    result: Some(json!({"verification":classify("", &[command], false)})),
                    ..Default::default()
                });
            }
        }
        let original: Vec<_> = jobs.iter().map(|job| job.result.clone()).collect();
        let scans = RefCell::new(Vec::new());
        let read = |path: std::path::PathBuf| {
            scans.borrow_mut().push(path.clone());
            async move { fingerprint(Arc::new(Workspace::open(&path).unwrap())).await }
        };
        let first = current_batch_using(&engine, &jobs, read).await.unwrap();
        assert!(first.iter().all(|summary| summary["status"] == "passed"));
        assert_eq!(
            scans.borrow().len(),
            2,
            "One scan per workspace, not per receipt"
        );
        // No engine event: an external rename plus a same-size write must be
        // observed by the next assessment, with no time-based cache reuse.
        std::fs::rename(
            root.path().join("a/source.txt"),
            root.path().join("a/renamed.txt"),
        )
        .unwrap();
        std::fs::write(root.path().join("a/renamed.txt"), "after!").unwrap();
        let second = current_batch_using(&engine, &jobs, read).await.unwrap();
        assert_eq!(scans.borrow().len(), 4);
        assert!(second[..2]
            .iter()
            .all(|summary| summary["status"] == "stale" && summary["verified"] == false));
        assert!(second[2..]
            .iter()
            .all(|summary| summary["status"] == "passed"));
        std::fs::remove_dir_all(root.path().join("b")).unwrap();
        let third = current_batch(&engine, &jobs).await.unwrap();
        assert!(third.iter().all(|summary| summary["status"] == "stale"));
        assert_eq!(
            jobs.iter()
                .map(|job| job.result.clone())
                .collect::<Vec<_>>(),
            original
        );
        engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn batch_refresh_does_not_scan_nonpassing_or_vendor_receipts() {
        let root = tempfile::tempdir().unwrap();
        let engine = crate::engine::Engine::open(
            crate::paths::AppPaths::isolated(&root.path().join("profile")).unwrap(),
        )
        .unwrap();
        let jobs: Vec<_> = [
            json!({"status":"vendor_owned","commands":[]}),
            classify("", &[check("failed check", State::Failed)], false),
            classify("", &[], false),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, summary)| crate::engine::Job {
            id: format!("job-{index}"),
            task_id: format!("task-{index}"),
            workspace: root.path().join("missing"),
            status: "completed".into(),
            result: Some(json!({"verification":summary})),
            ..Default::default()
        })
        .collect();
        let results = current_batch_using(&engine, &jobs, |_| async {
            panic!("Nonpassing historical evidence must not trigger a workspace scan")
        })
        .await
        .unwrap();
        assert_eq!(results[0]["status"], "vendor_owned");
        assert_eq!(results[1]["status"], "failed");
        assert_eq!(results[2]["status"], "not_run");
        assert!(current_batch(&engine, &[]).await.is_err());
        engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn legacy_inspection_scope_is_preserved_as_historical_not_revalidated() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let paths = crate::paths::AppPaths::isolated(&root.path().join("profile")).unwrap();
        let engine = crate::engine::Engine::open(paths).unwrap();
        let legacy = json!({"status":"not_run","claim":"observed","commands":[],
            "inspected_workspace":true,"verified":false});
        let job = crate::engine::Job {
            id: "attempt".into(),
            task_id: "task".into(),
            workspace: project,
            status: "completed".into(),
            result: Some(json!({"verification":legacy.clone()})),
            ..Default::default()
        };
        let assessed = current(&engine, &job).await.unwrap();
        assert_eq!(assessed["inspected_host"], false);
        assert_eq!(assessed["inspected_workspace"], true);
        assert_eq!(assessed["verified"], false);
        assert!(assessed["freshness_scope"]
            .as_str()
            .unwrap()
            .contains("historical and not revalidated"));
        assert_eq!(job.result.as_ref().unwrap()["verification"], legacy);
        engine.shutdown().await.unwrap();
    }

    fn check(command: &str, state: State) -> Value {
        serde_json::to_value(Receipt {
            schema_version: 1,
            task_id: "task".into(),
            attempt_id: "attempt".into(),
            tool_call_id: crate::id(),
            check_id: crate::workspace::hash(command.as_bytes()),
            workspace: "/project".into(),
            cwd: "/project".into(),
            command: command.into(),
            kind: Kind::ConfiguredCheck,
            success: state == State::Passed,
            exit_code: Some(if state == State::Passed { 0 } else { 1 }),
            state,
            provenance: "locally_observed".into(),
            scope: "configured check".into(),
            started_at: 1.0,
            finished_at: 2.0,
            process_seconds: Some(1.0),
            termination_reason: "exited".into(),
            workspace_fingerprint: Some("hash".into()),
            output_ref: "tool.completed:call".into(),
            timed_out: false,
        })
        .unwrap()
    }
    #[test]
    fn unrelated_success_never_erases_failure_and_same_check_can_recover() {
        let a = check("check A", State::Failed);
        let b = check("check B", State::Passed);
        assert_eq!(
            classify("All tests passed", &[a.clone(), b.clone()], false)["verified"],
            false
        );
        let result = classify("Done", &[a, b, check("check A", State::Passed)], false);
        assert_eq!(result["status"], "passed");
        assert_eq!(result["red_green"], true);
    }
    #[test]
    fn failed_execution_remains_visible_without_configured_checks() {
        let mut receipt = check("exit 7", State::Failed);
        receipt["kind"] = json!("command");
        let result = classify("Done", &[receipt], false);
        assert_eq!(result["status"], "not_run");
        assert_eq!(result["execution_failed"], true);
        assert_eq!(result["verified"], false);
    }

    #[test]
    fn legacy_claims_missing_fingerprints_and_nonpassing_receipts_never_verify() {
        for command in ["printf test", "echo lint", "cargo test", "pwd"] {
            assert_eq!(
                classify(
                    "All tests passed",
                    &[json!({"command":command,"success":true,"exit_code":0})],
                    false
                )["verified"],
                false
            );
        }
        for state in [
            State::Cancelled,
            State::Skipped,
            State::Stale,
            State::Running,
            State::NotRun,
        ] {
            assert_eq!(
                classify("Done", &[check("check A", state)], false)["verified"],
                false
            );
        }
        let mut receipt = check("check A", State::Passed);
        receipt["workspace_fingerprint"] = Value::Null;
        assert_eq!(classify("Done", &[receipt], false)["verified"], false);
    }

    #[tokio::test]
    async fn reassessment_keeps_passing_checks_observed_when_the_task_did_not_complete() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("source.txt"), "unchanged").unwrap();
        let paths = crate::paths::AppPaths::isolated(&root.path().join("profile")).unwrap();
        let engine = crate::engine::Engine::open(paths).unwrap();
        let workspace = Arc::new(Workspace::open(&project).unwrap());
        let observed_fingerprint = fingerprint(workspace).await.unwrap();
        // Seed retained receipts to exercise reassessment only. The engine
        // integration fixtures separately cover actual command execution.
        let receipt = |state| {
            let mut receipt = check("check A", state);
            receipt["workspace"] = json!(project);
            receipt["cwd"] = json!(project);
            receipt["workspace_fingerprint"] = json!(observed_fingerprint);
            receipt
        };
        for prior_failure in [false, true] {
            let mut commands = Vec::new();
            if prior_failure {
                commands.push(receipt(State::Failed));
            }
            commands.push(receipt(State::Passed));
            let original = classify("Done", &commands, true);
            assert_eq!(original["verified"], true);
            assert_eq!(original["claim"], "verified");
            assert_eq!(original["red_green"], prior_failure);
            for status in [
                "completed",
                "failed",
                "cancelled",
                "interrupted",
                "limit_reached",
            ] {
                let mut recorded = original.clone();
                if status != "completed" {
                    recorded["final_assessment"] = json!("not_completed");
                }
                let job = crate::engine::Job {
                    id: "attempt".into(),
                    task_id: "task".into(),
                    workspace: project.clone(),
                    status: status.into(),
                    result: Some(json!({"verification":recorded})),
                    ..Default::default()
                };
                let original_result = job.result.clone();
                let current = current(&engine, &job).await.unwrap();
                assert_eq!(current["commands"], json!(commands), "{status}");
                assert_eq!(current["verified"], status == "completed", "{status}");
                assert_eq!(
                    current["claim"],
                    if status == "completed" {
                        "verified"
                    } else {
                        "observed"
                    },
                    "{status}"
                );
                assert_eq!(
                    current["red_green"],
                    status == "completed" && prior_failure,
                    "{status}"
                );
                assert_eq!(
                    current["status"],
                    match status {
                        "completed" => "passed",
                        "failed" => "failed",
                        "cancelled" => "cancelled",
                        _ => "incomplete",
                    }
                );
                if status != "completed" {
                    assert_eq!(current["final_assessment"], "not_completed");
                }
                assert_eq!(
                    job.result, original_result,
                    "reassessment must not rewrite original receipts"
                );
            }
        }
        engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn fingerprint_observes_uncommitted_changes_renames_and_ignored_scope() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("source"), "before").unwrap();
        let workspace = Arc::new(Workspace::open(dir.path()).unwrap());
        let before = fingerprint(workspace.clone()).await.unwrap();
        std::fs::write(dir.path().join("source"), "after").unwrap();
        let after = fingerprint(workspace.clone()).await.unwrap();
        assert_ne!(before, after);
        std::fs::rename(dir.path().join("source"), dir.path().join("renamed")).unwrap();
        let renamed = fingerprint(workspace.clone()).await.unwrap();
        assert_ne!(after, renamed);
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/generated"), "ignored").unwrap();
        assert_eq!(fingerprint(workspace.clone()).await.unwrap(), renamed);
        let mut receipt = check("check A", State::Passed);
        receipt["workspace_fingerprint"] = json!(before);
        let mut receipts = vec![receipt];
        refresh(&mut receipts, workspace).await;
        assert_eq!(receipts[0]["state"], "stale");
    }
}

//! Opt-in subscription CLI coding acceptance fixture; never an automatic test.
//! Usage: cargo run -p shadowcode-core --example live_vendor_coding -- report.json cli:codex [cli:claude ...]
//! Offline receipt replay: live_vendor_coding --replay saved-report.json new-replay.json
//! Supports explicit cli:{codex,claude,cursor,antigravity,grok}[:model] targets.
//! Uses existing vendor login and vendor-owned tools/sandbox, with isolated
//! ShadowCode profiles/projects. No login, install, user-config patch or API
//! route fallback. Vendor login/session caches may be updated by the CLI.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use shadowcode_core::{
    approvals::Approval,
    cli_agent::{self, antigravity_server, picker::Availability, Vendor},
    config::Config,
    engine::Job,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[path = "support/coding_fixture.rs"]
mod coding_fixture;
use coding_fixture::{
    assertions_passed, bounded_regular_text, check, CHECK_COMMAND, SOURCE, TASK, TESTS,
};

const TASK_SECONDS: u64 = 240;
const EVENT_BYTES: usize = 1_000_000;
const EVENT_COUNT: usize = 2_000;
const LIMITATIONS: &str = "Vendor CLIs own their tools, sandbox, home access and network. ShadowCode's Bubblewrap sandbox applies only to independent grading. Only approval requests actually forwarded by the vendor can be filtered here; an isolated working directory is not a vendor filesystem sandbox. Existing CLI login/session caches may change. ACP advertised auth methods and successful session initialization do not prove billing source. No fallback or API route is requested. Missing exact test-command receipts fail acceptance even when independent tests pass. This fixed Python grader is not adversarial-proof.";

fn vendor_for(target: &str) -> Result<Vendor> {
    let config = cli_agent::resolve_vendor(target)
        .context("Expected an explicit supported cli:<vendor>[:model] target")?;
    Vendor::from_provider(&config.provider).context("Not a supported vendor route")
}

fn preflight_environment(vendor: Vendor) -> Result<()> {
    // Inspect presence only, never credential contents. Refuse ambiguous key
    // overrides instead of mutating the process environment or user settings.
    let names: &[&str] = match vendor {
        Vendor::Codex => &["OPENAI_API_KEY", "CODEX_API_KEY", "OPENAI_BASE_URL"],
        Vendor::Claude => &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
        ],
        Vendor::Cursor => &["CURSOR_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY"],
        Vendor::Antigravity => &[
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GOOGLE_GENAI_USE_VERTEXAI",
        ],
        Vendor::Grok => &["GROK_API_KEY", "XAI_API_KEY", "OPENAI_API_KEY"],
    };
    for name in names {
        ensure!(
            std::env::var_os(name).is_none(),
            "Refusing live subscription fixture while {name} is set; no value was inspected"
        );
    }
    if vendor == Vendor::Antigravity {
        // The adapter initializes personal auth settings if absent/different.
        // Require them to exist already, so this fixture never requests setup.
        let settings = antigravity_server::profile_dir().join("antigravity-acp/settings.json");
        let value: Value = serde_json::from_str(&bounded_regular_text(&settings, 100_000)?)?;
        ensure!(value["auth"]["type"] == antigravity_server::AUTH_METHOD,
            "Antigravity must already have its managed personal-login profile configured; fixture does not set it up");
    }
    Ok(())
}

fn fixture_path(project: &Path, candidate: &str, edit: bool) -> bool {
    let allowed = |name: &str| name == "helpers.py" || (!edit && name == "test_helpers.py");
    let path = Path::new(candidate);
    let relative = if path.is_absolute() {
        let Ok(relative) = path.strip_prefix(project) else {
            return false;
        };
        relative
    } else {
        path
    };
    let mut components = relative
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir));
    matches!((components.next(), components.next()), (Some(std::path::Component::Normal(name)), None) if name.to_str().is_some_and(allowed))
        && fs::symlink_metadata(project.join(relative)).is_ok_and(|m| m.is_file())
}

fn cwd_matches(arguments: &Value, tool: &str, project: &Path) -> bool {
    [
        arguments.get("cwd"),
        arguments.pointer("/input/cwd"),
        arguments.pointer("/input/Cwd"),
        arguments.pointer("/input/working_directory"),
        (tool == "antigravity.execute")
            .then(|| arguments.pointer("/input/working_dir"))
            .flatten(),
    ]
    .into_iter()
    .flatten()
    .all(|v| v.is_null() || matches!(v.as_str(), Some("" | ".")) || v.as_str() == project.to_str())
}

fn tool_command<'a>(arguments: &'a Value, tool: &str) -> Option<&'a str> {
    arguments["command"]
        .as_str()
        .or(arguments["input"]["command"].as_str())
        .or_else(|| {
            (tool == "antigravity.execute")
                .then(|| {
                    arguments["input"]["CommandLine"]
                        .as_str()
                        .or(arguments["input"]["command_line"].as_str())
                })
                .flatten()
        })
}

fn approval_allowed(approval: &Approval, project: &Path) -> bool {
    if !cwd_matches(&approval.arguments, &approval.tool, project) {
        return false;
    }
    let tool = approval.tool.as_str();
    let command_tool = tool == "exec"
        || tool.ends_with(".command_execution")
        || tool.ends_with(".Bash")
        || tool.ends_with(".execute");
    if command_tool {
        return tool_command(&approval.arguments, tool).is_some_and(|command| {
            matches!(
                command.trim(),
                CHECK_COMMAND
                    | "cat helpers.py"
                    | "cat test_helpers.py"
                    | "cat helpers.py test_helpers.py"
                    | "pwd"
                    | "ls"
            )
        });
    }
    let edit = tool.ends_with(".edit")
        || tool.ends_with(".file_change")
        || matches!(tool, "claude.Edit" | "claude.Write" | "claude.MultiEdit");
    let read = tool.ends_with(".read") || tool == "claude.Read";
    if !edit && !read {
        return false;
    }
    let mut paths = Vec::new();
    collect_paths(&approval.arguments, &mut paths);
    // Opaque grant-root/permission requests and unrecognized tools are denied.
    !paths.is_empty() && paths.iter().all(|path| fixture_path(project, path, edit))
}

fn collect_paths<'a>(value: &'a Value, paths: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "path" | "file_path" | "filePath" | "target_file" | "TargetFile"
                ) {
                    if let Some(path) = value.as_str() {
                        paths.push(path);
                    }
                } else if key == "changes" {
                    if let Some(changes) = value.as_object() {
                        paths.extend(changes.keys().map(String::as_str));
                    }
                } else if matches!(key.as_str(), "input" | "locations" | "content" | "edits") {
                    collect_paths(value, paths);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_paths(item, paths);
            }
        }
        _ => {}
    }
}

fn bounded_value(value: &Value) -> Value {
    // Reports are evidence snapshots, not an unbounded copy of provider output.
    match value {
        Value::String(text) => json!(if text.len() > 8_000 {
            format!(
                "{} [report string truncated]",
                text.chars().take(2_000).collect::<String>()
            )
        } else {
            text.clone()
        }),
        Value::Array(items) => Value::Array(items.iter().take(128).map(bounded_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .take(128)
                .map(|(key, value)| (key.clone(), bounded_value(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn events(service: &Service, session: &str) -> Result<(Vec<Value>, bool)> {
    let mut result = Vec::new();
    let mut cursor = 0;
    let mut bytes = 0;
    loop {
        let page = service
            .engine
            .store()
            .events_after(session, cursor, None, 64)?;
        if page.is_empty() {
            return Ok((result, false));
        }
        for event in page {
            let size = serde_json::to_vec(&event)?.len();
            if result.len() == EVENT_COUNT || bytes + size > EVENT_BYTES {
                return Ok((result, true));
            }
            cursor = event["id"].as_i64().context("Event cursor missing")?;
            bytes += size;
            result.push(event);
        }
    }
}

fn antigravity_raw_test_output<'a>(output: &'a Value, project: &Path) -> Option<&'a str> {
    // Observed in the 2026-09-26 capture, not inferred from model prose. Keep
    // this narrow until another real provider shape is independently checked.
    if output["raw_output_format"] != "json"
        || output["raw_output_truncated"] != false
        || output["input"]["command_line"] != CHECK_COMMAND
        || output["input"]["working_dir"].as_str() != project.to_str()
    {
        return None;
    }
    let raw = output["raw_output"].as_object()?;
    if raw.len() != 6
        || raw.get("exitCode")?.as_i64() != Some(0)
        || raw.get("exit_code")?.as_i64() != Some(0)
        || raw.get("commandLine")?.as_str() != Some(CHECK_COMMAND)
        || raw.get("workingDir")?.as_str() != project.to_str()
    {
        return None;
    }
    let text = raw.get("combinedOutput")?.as_str()?;
    if raw.get("formatted_output")?.as_str() != Some(text)
        || !output["content"].as_array()?.iter().all(|content| {
            content
                .as_str()
                .is_some_and(|content| content.is_empty() || content == text)
        })
    {
        // Retained denials or other conflicting content cannot be silently
        // replaced by a successful-looking structured result.
        return None;
    }
    let lines: Vec<_> = text.lines().filter(|line| !line.is_empty()).collect();
    if lines.len() != 3
        || lines[0].len() < 3
        || !lines[0].bytes().all(|byte| byte == b'-')
        || lines[2] != "OK"
    {
        return None;
    }
    let seconds = lines[1]
        .strip_prefix("Ran 5 tests in ")?
        .strip_suffix('s')?;
    if !seconds
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
        || !seconds
            .parse::<f64>()
            .is_ok_and(|seconds| seconds.is_finite() && seconds >= 0.0)
    {
        return None;
    }
    Some(text)
}

fn test_receipt(
    events: &[Value],
    vendor: Vendor,
    project: &Path,
    session_id: &str,
    task_id: &str,
) -> Option<Value> {
    if session_id.is_empty() || task_id.is_empty() {
        return None;
    }
    let acp = matches!(vendor, Vendor::Cursor | Vendor::Grok | Vendor::Antigravity);
    let mut started = HashMap::new();
    let mut seen = HashSet::new();
    for event in events {
        if event["session_id"].as_str() != Some(session_id)
            || event["task_id"].as_str() != Some(task_id)
        {
            continue;
        }
        let payload = &event["payload"];
        let id = payload["call_id"].as_str().unwrap_or("");
        let tool = payload["tool"].as_str().unwrap_or("");
        if id.is_empty()
            || id.contains(shadowcode_core::redaction::placeholder())
            || !tool.starts_with(&format!("{}.", vendor.id()))
        {
            continue;
        }
        if event["type"] == "tool.started" {
            if !seen.insert(id) {
                // Reused identifiers cannot establish an unambiguous pair.
                started.remove(id);
                continue;
            }
            let command_tool = tool.ends_with(".command_execution") || tool.ends_with(".Bash");
            if acp
                || (command_tool
                    && cwd_matches(&payload["arguments"], tool, project)
                    && tool_command(&payload["arguments"], tool)
                        .is_some_and(|command| is_test_command(command, vendor)))
            {
                started.insert(id, tool);
            }
        } else if event["type"] == "tool.completed" {
            let Some(initial_tool) = started.remove(id) else {
                continue;
            };
            if payload["success"] != true
                || payload
                    .get("error")
                    .is_some_and(|error| !error.is_null() && error != "")
            {
                continue;
            }
            if acp {
                // ACP initially permits generic tool kinds and partial input.
                // Completion carries the adapter's merged current metadata;
                // never attribute an old start command to a changed operation.
                let latest = json!({"input":payload["output"]["input"]});
                if tool != format!("{}.execute", vendor.id())
                    || payload["output"]["tool_kind"] != "execute"
                    || payload["output"]["status"] != "completed"
                    || payload["output"]["content"]
                        .as_array()
                        .is_some_and(|content| {
                            content.iter().filter_map(Value::as_str).any(|text| {
                                let text = text.to_ascii_lowercase();
                                [
                                    "not in allowlist:",
                                    "denied by user",
                                    "rejected by user",
                                    "tool execution failed",
                                ]
                                .iter()
                                .any(|denial| text.contains(denial))
                            })
                        })
                    || !cwd_matches(&latest, tool, project)
                    || tool_command(&latest, tool)
                        .is_none_or(|command| command.trim() != CHECK_COMMAND)
                {
                    continue;
                }
            } else if initial_tool != tool {
                continue;
            }
            // Only captured tool output is execution evidence. Typed input,
            // descriptions and titles can also contain apparent test results.
            let mut source = "captured_content";
            let output = if vendor == Vendor::Antigravity
                && payload["output"].get("raw_output").is_some()
            {
                source = "antigravity_structured_raw_output";
                let Some(text) = antigravity_raw_test_output(&payload["output"], project) else {
                    continue;
                };
                text.to_owned()
            } else if acp {
                payload["output"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                payload["output"]["output"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned()
            };
            // A typed completion plus recognizable unittest output, not the
            // model's final prose. Independent grading is still required.
            if output.contains("Ran 5 tests") && output.contains("OK") && !output.contains("FAILED")
            {
                return Some(
                    json!({"call_id":id,"session_id":session_id,"task_id":task_id,"tool":tool,"command":CHECK_COMMAND,"source":source,"completion":bounded_value(payload)}),
                );
            }
        }
    }
    None
}

fn is_test_command(command: &str, vendor: Vendor) -> bool {
    let command = command.trim();
    command == CHECK_COMMAND
        // Exact official Codex wrapper observed in the live fixture. This is
        // a finite string match, not shell parsing or permission to run it.
        // Approval matching intentionally remains on the bare command only.
        || (vendor == Vendor::Codex
            && command == "/bin/bash -lc 'python3 -m unittest -q'")
}

fn alias_report_call_ids(report: &mut Value) {
    fn visit(value: &mut Value, aliases: &mut HashMap<String, String>, collapsed: &mut usize) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if matches!(key.as_str(), "call_id" | "tool_call_id" | "item_id") {
                        if let Some(id) = value.as_str().filter(|id| !id.is_empty()) {
                            if id.contains(shadowcode_core::redaction::placeholder()) {
                                *collapsed += 1;
                            } else {
                                let next = format!("fixture-call-{}", aliases.len() + 1);
                                let alias = aliases.entry(id.to_owned()).or_insert(next);
                                *value = json!(alias);
                            }
                        }
                    } else {
                        visit(value, aliases, collapsed);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    visit(item, aliases, collapsed);
                }
            }
            _ => {}
        }
    }
    let mut aliases = HashMap::new();
    let mut collapsed = 0;
    visit(report, &mut aliases, &mut collapsed);
    report["call_id_handling"] = json!({
        "internal_matching":"original store identifiers before report sanitization",
        "report_identifiers":"stable distinct fixture-call-N aliases within this result",
        "distinct_identifiers":aliases.len(), "already_redacted_fields":collapsed
    });
}

async fn evaluate(target: &str) -> Result<Value> {
    let root = tempfile::tempdir()?;
    let project = root.path().join("project");
    let mut report = json!({"target":target,"passed":false,"fixture_task":TASK,"fixture_provenance":coding_fixture::provenance(),"limitations":LIMITATIONS});
    let mut service: Option<Service> = None;
    let mut approvals = None;
    let decisions = Arc::new(Mutex::new(Vec::new()));
    let started = Instant::now();
    let outcome: Result<()> = async {
        let vendor = vendor_for(target)?;
        preflight_environment(vendor)?;
        fs::create_dir(&project)?;
        fs::write(project.join("helpers.py"), SOURCE)?;
        fs::write(project.join("test_helpers.py"), TESTS)?;
        let baseline = check(&project).await?;
        let baseline_valid = !baseline.ok && !baseline.truncated && !baseline.timed_out
            && baseline.stdout.contains("SHADOWCODE_EVALUATOR:FAIL") && baseline.stderr.contains("Ran 5 tests");
        report["baseline_check"] = json!(baseline);
        ensure!(baseline_valid, "Baseline did not demonstrably execute and fail the fixed five-test suite");
        let paths = AppPaths::isolated(&root.path().join("profile"))?;
        Config::patch(&paths, json!({
            "model":{"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"unused"},
            "trusted_workspaces":[project], "network":{"mode":"online"},
            "permissions":{"mode":"ask","approve_shell":true,"network":false},
            "sandbox":{"require":true,"home_binds":[]},
            "limits":{"on_limit":"ask","fallback_model":""},
            "cli_agents":{"enabled":true,"approval_timeout_sec":30,"stall_timeout_sec":60,"max_run_time_sec":TASK_SECONDS},
            "agent":{"max_steps":14,"max_task_tokens":50000,"tool_timeout_sec":30}
        }))?;
        let config = Config::load(&paths, Some(&project))?;
        report["configured_cli_binary"] = json!(config.cli_agents.binary(vendor));
        service = Some(Service::open(paths, Some(project.clone()))?);
        let service = service.as_ref().expect("opened service");
        // Refresh only the requested vendor; a full picker probes unrelated
        // accounts and may initialize Antigravity settings for another route.
        let status = tokio::time::timeout(Duration::from_secs(60), service.engine.vendors().refresh(vendor, &config.cli_agents, false)).await.context("Vendor preflight exceeded 60 seconds")?;
        report["vendor_status"] = json!({"vendor":status.vendor,"availability":status.availability,"version":status.version,"binary":status.binary,"detail":status.detail,"error":status.error,"auth_mode":status.account.as_ref().and_then(|a| a.auth_mode.as_deref()),"models":status.models});
        ensure!(status.availability == Availability::Ready, "Requested vendor is not ready; no login or setup attempted");
        ensure!(!status.api_key_login(), "Refusing an API-key CLI login for a subscription acceptance fixture");
        let auth = status.account.as_ref().and_then(|a| a.auth_mode.as_deref());
        ensure!(match vendor { Vendor::Codex => auth == Some("chatgpt"), Vendor::Claude => auth == Some("claude.ai"), _ => !auth.is_some_and(|a| a.to_ascii_lowercase().contains("api")) }, "Subscription authentication could not be established for this route");
        let model = cli_agent::resolve_vendor(target).expect("validated target");
        ensure!(model.name == "default" || status.models.iter().any(|m| m.id == model.name), "Requested model is absent from the vendor catalog");
        let usage = status.usage_for(&model.name, shadowcode_core::now());
        ensure!(!usage.limit_reached, "Requested model is at its reported subscription limit");
        report["selected_route"] = cli_agent::picker::vendor_target(vendor, &model.name, &model.name, status.availability, &status.detail, usage, false).to_json();
        report["auth_evidence"] = json!(if matches!(vendor, Vendor::Codex | Vendor::Claude) { "official CLI reports subscription auth mode" } else { "ACP session initialized with existing login; advertised auth method is not billing proof" });
        let controller = service.engine.clone();
        let approved_project = project.clone();
        let recorded = decisions.clone();
        approvals = Some(tokio::spawn(async move {
            loop {
                for approval in controller.approvals().list(None) {
                    let allow = approval_allowed(&approval, &approved_project);
                    let result = controller.approvals().decide(&approval.id, &approval.session_id, allow);
                    let mut records = recorded.lock().expect("approval records");
                    if records.len() < 128 { records.push(json!({"tool":approval.tool,"command":approval.command.chars().take(1000).collect::<String>(),"allowed":allow,"decision_ok":result.is_ok()})); }
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        }));
        let job: Job = serde_json::from_value(tokio::time::timeout(Duration::from_secs(30), service.dispatch(Request {
            method:"POST".into(),path:"/api/jobs".into(),body:json!({"workspace":project,"model":target,"purpose":"code","web":false,"task":TASK})
        })).await.context("Job submission exceeded 30 seconds")??)?;
        report["submitted_job"] = json!({"id":job.id,"model":job.model,"session_id":job.session_id});
        let deadline = tokio::time::sleep(Duration::from_secs(TASK_SECONDS));
        tokio::pin!(deadline);
        let done = tokio::select! {
            done = service.engine.wait(&job.id) => Some(done),
            _ = &mut deadline => { report["deadline_exceeded"] = json!(true); None },
            _ = tokio::signal::ctrl_c() => { report["interrupted"] = json!(true); None }
        };
        let done = if let Some(done) = done { done } else {
            let cancel = tokio::time::timeout(Duration::from_secs(15), service.engine.cancel(&job.id)).await;
            report["cancel_result"] = json!(match cancel { Ok(Ok(job)) => job.status, Ok(Err(e)) => format!("{e:#}"), Err(_) => "cancel exceeded 15 seconds".into() });
            tokio::time::timeout(Duration::from_secs(5), service.engine.wait(&job.id)).await.context("Cancelled job did not settle within 5 seconds").and_then(|r| r)
        };
        let completed = match done {
            Ok(done) => { let completed = done.status == "completed" && done.model == job.model; report["job"] = bounded_value(&json!(done)); completed },
            Err(error) => { report["job_wait_error"] = json!(format!("{error:#}")); false }
        };
        let (observed, truncated) = events(service, &job.session_id)?;
        let receipt = test_receipt(&observed, vendor, &project, &job.session_id, &job.task_id);
        let routed = observed.iter().any(|e| e["type"] == "agent.started" && e["payload"]["vendor_agent"] == vendor.id());
        let fallback = observed.iter().any(|e| matches!(e["type"].as_str(), Some("routing.fallback" | "limit.fallback")));
        report["vendor_test_receipt"] = json!(receipt);
        report["events_truncated"] = json!(truncated);
        report["events"] = json!(observed);
        report["observed_vendor_route"] = json!(routed);
        report["fallback_observed"] = json!(fallback);
        let preserved = bounded_regular_text(&project.join("test_helpers.py"), 100_000).is_ok_and(|text| text == TESTS);
        report["tests_preserved"] = json!(preserved);
        let independent = check(&project).await?;
        let passed = assertions_passed(&independent);
        report["independent_check"] = json!(independent);
        report["independent_assertions_passed"] = json!(passed);
        report["source_after"] = json!(bounded_regular_text(&project.join("helpers.py"), 100_000)?);
        report["passed"] = json!(completed && routed && !fallback && !truncated && preserved && passed && receipt.is_some() && report["deadline_exceeded"] != true && report["interrupted"] != true);
        Ok(())
    }.await;
    if let Some(task) = approvals {
        task.abort();
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }
    if let Some(service) = service {
        let cleanup =
            tokio::time::timeout(Duration::from_secs(15), service.engine.shutdown()).await;
        let error = match cleanup {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!("{e:#}")),
            Err(_) => Some("shutdown exceeded 15 seconds".into()),
        };
        if let Some(error) = error {
            report["cleanup_error"] = json!(error);
            report["passed"] = json!(false);
        }
    }
    if let Err(error) = outcome {
        report["error"] = json!(format!("{error:#}"));
        report["passed"] = json!(false);
    }
    report["approval_decisions"] = json!(*decisions.lock().expect("approval records"));
    report["elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
    alias_report_call_ids(&mut report);
    shadowcode_core::redaction::redact_value(&mut report);
    Ok(report)
}

fn replay_receipts(capture: &Value) -> Result<Value> {
    let runs = capture["results"]
        .as_array()
        .context("Missing captured results")?;
    ensure!(
        !runs.is_empty() && runs.len() <= 5,
        "Expected one to five recorded results"
    );
    let mut results = Vec::new();
    for run in runs {
        let target = run["target"].as_str().context("Missing captured target")?;
        let vendor = vendor_for(target)?;
        ensure!(
            run["fixture_task"] == TASK,
            "Captured fixture task differs from this grader"
        );
        for key in [
            "task_sha256",
            "source_sha256",
            "tests_sha256",
            "grader_source_sha256",
        ] {
            ensure!(
                run["fixture_provenance"][key] == coding_fixture::provenance()[key],
                "Captured fixture provenance differs: {key}"
            );
        }
        ensure!(
            run["events_truncated"] == false,
            "Cannot replay a truncated event capture"
        );
        ensure!(
            run["call_id_handling"]["report_identifiers"]
                == "stable distinct fixture-call-N aliases within this result",
            "Captured call-ID alias provenance is missing"
        );
        let events = run["events"]
            .as_array()
            .context("Missing captured events")?;
        ensure!(
            events.len() <= EVENT_COUNT && serde_json::to_vec(events)?.len() <= EVENT_BYTES,
            "Captured events exceed fixture bounds"
        );
        let job = &run["job"];
        let project = Path::new(
            job["workspace"]
                .as_str()
                .context("Missing captured workspace")?,
        );
        ensure!(project.is_absolute(), "Captured workspace must be absolute");
        let session = job["session_id"]
            .as_str()
            .context("Missing captured job session")?;
        let task = job["task_id"]
            .as_str()
            .context("Missing captured job task")?;
        ensure!(
            !session.is_empty() && !task.is_empty(),
            "Captured job scope is empty"
        );
        ensure!(
            job["session_id"] == run["submitted_job"]["session_id"]
                && job["id"] == run["submitted_job"]["id"],
            "Captured submitted/final job scope differs"
        );
        let receipt = test_receipt(events, vendor, project, session, task);
        results.push(json!({
            "target":target, "original_live_passed":run["passed"],
            "receipt_accepted":receipt.is_some(), "replayed_receipt":receipt,
            "recorded_independent_assertions_passed":run["independent_assertions_passed"],
            // This count can include unrelated nested approval IDs. Receipt
            // matching itself rejects collapsed or duplicate event call IDs.
            "recorded_collapsed_id_fields":run["call_id_handling"]["already_redacted_fields"],
            "original_live_result":run,
        }));
    }
    Ok(json!({
        "mode":"offline_receipt_replay", "provider_calls":0,
        "scope":"Re-evaluates saved tool receipts only. Original live verdicts are preserved; no new model turn or independent Python grading was run.",
        "grader_example_sha256":shadowcode_core::workspace::hash(include_str!("live_vendor_coding.rs").as_bytes()),
        "results":results,
    }))
}

fn replay_file(source: &Path, destination: &Path) -> Result<()> {
    let text = bounded_regular_text(source, 12_000_000)?;
    let mut replay = replay_receipts(&serde_json::from_str(&text)?)?;
    replay["source_capture_sha256"] = json!(shadowcode_core::workspace::hash(text.as_bytes()));
    replay["source_capture_path"] = json!(source.canonicalize()?);
    // A replay is a new artifact. Never overwrite a live report or an earlier
    // replay, even when the caller accidentally gives the same path twice.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(&replay)?)?;
    for result in replay["results"].as_array().expect("replay results") {
        println!(
            "{} original_live_passed={} offline_receipt_accepted={}",
            result["target"], result["original_live_passed"], result["receipt_accepted"]
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--replay") {
        ensure!(
            args.len() == 3,
            "usage: live_vendor_coding --replay saved-report.json new-replay.json"
        );
        return replay_file(Path::new(&args[1]), Path::new(&args[2]));
    }
    ensure!(
        (2..=6).contains(&args.len()),
        "usage: live_vendor_coding report.json cli:<vendor>[:model] [up to five explicit targets]"
    );
    for target in &args[1..] {
        vendor_for(target)?;
    }
    let mut reports = Vec::new();
    for target in &args[1..] {
        let report = evaluate(target).await.unwrap_or_else(
            |error| json!({"target":target,"passed":false,"error":format!("{error:#}")}),
        );
        println!(
            "{target} passed={} status={} error={}",
            report["passed"], report["job"]["status"], report["error"]
        );
        let interrupted = report["interrupted"] == true;
        reports.push(report);
        let report = json!({"scope":"One shared small Python repair fixture per explicit subscription CLI route; not a general coding benchmark", "task_timeout_seconds":TASK_SECONDS,"grader_timeout_seconds":30,"max_event_bytes":EVENT_BYTES,"max_events":EVENT_COUNT,"limitations":LIMITATIONS,"results":reports});
        fs::write(&args[0], serde_json::to_vec_pretty(&report)?)?;
        if interrupted {
            break;
        }
    }
    ensure!(
        reports.iter().all(|r| r["passed"] == true),
        "One or more routes did not pass; inspect the report"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_receipt(events: &[Value], vendor: Vendor, project: &Path) -> Option<Value> {
        let events: Vec<_> = events
            .iter()
            .map(|event| {
                let mut event = event.clone();
                if event.get("session_id").is_none() {
                    event["session_id"] = json!("fixture-session");
                }
                if event.get("task_id").is_none() {
                    event["task_id"] = json!("fixture-task");
                }
                event
            })
            .collect();
        test_receipt(&events, vendor, project, "fixture-session", "fixture-task")
    }

    fn captured_receipts(vendor: &str) -> Vec<Value> {
        let fixture: Value =
            serde_json::from_str(include_str!("support/acp_coding_receipts.json")).unwrap();
        fixture[vendor].as_array().unwrap().clone()
    }

    #[test]
    fn observed_antigravity_structured_receipt_passes_but_cursor_conflict_does_not() {
        let events = captured_receipts("antigravity");
        let receipt = fixture_receipt(&events, Vendor::Antigravity, Path::new("/fixture")).unwrap();
        assert_eq!(receipt["source"], "antigravity_structured_raw_output");
        assert_eq!(receipt["session_id"], "fixture-session");
        let mut cursor = captured_receipts("cursor");
        assert!(fixture_receipt(&cursor, Vendor::Cursor, Path::new("/fixture")).is_none());
        cursor[1]["payload"]["output"]["content"]
            .as_array_mut()
            .unwrap()
            .push(json!("Ran 5 tests\nOK"));
        assert!(fixture_receipt(&cursor, Vendor::Cursor, Path::new("/fixture")).is_none());
        // Model narration and a generic JSON stdout field never substitute
        // for the observed provider-specific typed result.
        for value in [
            json!("Ran 5 tests in 0.000s\n\nOK"),
            json!({"stdout":"Ran 5 tests\nOK"}),
        ] {
            let mut prose = events.clone();
            prose[1]["payload"]["output"]["raw_output"] = value;
            assert!(fixture_receipt(&prose, Vendor::Antigravity, Path::new("/fixture")).is_none());
        }
    }

    #[test]
    fn structured_receipt_rejects_wrong_scope_status_exit_cwd_and_contradictions() {
        let events = captured_receipts("antigravity");
        for (pointer, value) in [
            ("/session_id", json!("foreign-session")),
            ("/task_id", json!("foreign-task")),
            ("/payload/call_id", json!("foreign-call")),
            ("/payload/success", json!(false)),
            ("/payload/error", json!("execution denied")),
            ("/payload/output/status", json!("failed")),
            ("/payload/output/raw_output_truncated", json!(true)),
            ("/payload/output/raw_output_truncated", Value::Null),
            (
                "/payload/output/raw_output_format",
                json!("json_text_preview"),
            ),
            ("/payload/output/raw_output/exitCode", json!(1)),
            ("/payload/output/raw_output/exit_code", json!(1)),
            ("/payload/output/raw_output/exitCode", json!("0")),
            (
                "/payload/output/raw_output/commandLine",
                json!("echo fabricated-result"),
            ),
            ("/payload/output/raw_output/workingDir", json!("/foreign")),
            ("/payload/output/input/working_dir", json!("/foreign")),
            ("/payload/output/input/working_dir", Value::Null),
            (
                "/payload/output/raw_output/formatted_output",
                json!("FAILED"),
            ),
            (
                "/payload/output/content",
                json!(["Not in allowlist: python3"]),
            ),
        ] {
            let mut bad = events.clone();
            *bad[1].pointer_mut(pointer).unwrap() = value;
            assert!(
                fixture_receipt(&bad, Vendor::Antigravity, Path::new("/fixture")).is_none(),
                "accepted {pointer}"
            );
        }
        for summary in [
            "Ran 5 tests\nOK",
            "The model says Ran 5 tests in 0.000s\nOK",
            "---\nRan 0 tests in 0.000s\nOK",
            "---\nRan 5 tests in 0.000s\nOK\nFAILED",
        ] {
            let mut bad = events.clone();
            bad[1]["payload"]["output"]["raw_output"]["combinedOutput"] = json!(summary);
            bad[1]["payload"]["output"]["raw_output"]["formatted_output"] = json!(summary);
            assert!(fixture_receipt(&bad, Vendor::Antigravity, Path::new("/fixture")).is_none());
        }
        let mut cross_start = events.clone();
        cross_start[0]["task_id"] = json!("foreign-task");
        assert!(
            fixture_receipt(&cross_start, Vendor::Antigravity, Path::new("/fixture")).is_none()
        );
        assert!(fixture_receipt(
            &[events[0].clone(), events[0].clone(), events[1].clone()],
            Vendor::Antigravity,
            Path::new("/fixture")
        )
        .is_none());
        assert!(test_receipt(
            &events,
            Vendor::Antigravity,
            Path::new("/fixture"),
            "",
            "fixture-task"
        )
        .is_none());
    }

    #[test]
    fn replay_preserves_original_failure_and_never_overwrites_source() {
        let capture = json!({"results":[{
            "target":"cli:antigravity:gemini-3.8-flash-high", "passed":false,
            "fixture_task":TASK,"fixture_provenance":coding_fixture::provenance(),
            "events_truncated":false,"call_id_handling":{"already_redacted_fields":1,"report_identifiers":"stable distinct fixture-call-N aliases within this result"},
            "events":captured_receipts("antigravity"),
            "job":{"id":"job","workspace":"/fixture","session_id":"fixture-session","task_id":"fixture-task"},
            "submitted_job":{"id":"job","session_id":"fixture-session"},
            "independent_assertions_passed":true,"vendor_test_receipt":null
        }]});
        let replay = replay_receipts(&capture).unwrap();
        assert_eq!(replay["mode"], "offline_receipt_replay");
        assert_eq!(replay["provider_calls"], 0);
        assert_eq!(replay["results"][0]["original_live_passed"], false);
        assert_eq!(replay["results"][0]["receipt_accepted"], true);
        assert_eq!(
            replay["results"][0]["original_live_result"],
            capture["results"][0]
        );
        assert!(replay["results"][0].get("passed").is_none());
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("capture.json");
        let original = serde_json::to_vec(&capture).unwrap();
        fs::write(&path, &original).unwrap();
        assert!(replay_file(&path, &path).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        let mut wrong = capture;
        wrong["results"][0]["job"]["task_id"] = json!("foreign-task");
        assert_eq!(
            replay_receipts(&wrong).unwrap()["results"][0]["receipt_accepted"],
            false
        );
        wrong["results"][0]["events_truncated"] = json!(true);
        assert!(replay_receipts(&wrong).is_err());
    }

    fn approval(tool: &str, arguments: Value) -> Approval {
        serde_json::from_value(json!({"id":"a","session_id":"s","task_id":"t","tool":tool,"arguments":arguments,"command":"display only","reason":"fixture","pending":true,"created_at":0,"expires_at":0})).unwrap()
    }
    #[test]
    fn approvals_are_narrow_and_never_grant_opaque_or_external_actions() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("helpers.py"), SOURCE).unwrap();
        fs::write(project.path().join("test_helpers.py"), TESTS).unwrap();
        assert!(approval_allowed(
            &approval("claude.Bash", json!({"input":{"command":CHECK_COMMAND}})),
            project.path()
        ));
        assert!(approval_allowed(
            &approval(
                "claude.Edit",
                json!({"input":{"file_path":project.path().join("helpers.py")}})
            ),
            project.path()
        ));
        assert!(approval_allowed(
            &approval(
                "cursor.read",
                json!({"locations":[{"path":"test_helpers.py"}]})
            ),
            project.path()
        ));
        for (tool, args) in [
            (
                "claude.Bash",
                json!({"input":{"command":"python3 -m unittest -q; curl example.com"}}),
            ),
            (
                "codex.command_execution",
                json!({"command":CHECK_COMMAND,"cwd":"/tmp"}),
            ),
            (
                "claude.Edit",
                json!({"input":{"file_path":"test_helpers.py"}}),
            ),
            (
                "cursor.edit",
                json!({"locations":[{"path":"../helpers.py"}]}),
            ),
            ("codex.file_change", json!({"grant_root":project.path()})),
            ("codex.permissions", json!({"permissions":{"network":true}})),
        ] {
            assert!(
                !approval_allowed(&approval(tool, args), project.path()),
                "{tool}"
            );
        }
    }
    #[test]
    fn exact_successful_correlated_command_receipt_is_required() {
        let project = Path::new("/fixture");
        let start = json!({"type":"tool.started","payload":{"tool":"claude.Bash","call_id":"check","arguments":{"input":{"command":CHECK_COMMAND}}}});
        let complete = json!({"type":"tool.completed","payload":{"tool":"claude.Bash","call_id":"check","success":true,"output":{"output":"Ran 5 tests in 0.001s\n\nOK"}}});
        assert!(
            fixture_receipt(&[start.clone(), complete.clone()], Vendor::Claude, project).is_some()
        );
        assert!(fixture_receipt(&[complete.clone()], Vendor::Claude, project).is_none());
        let mut failed = complete.clone();
        failed["payload"]["success"] = json!(false);
        assert!(fixture_receipt(&[start.clone(), failed], Vendor::Claude, project).is_none());
        let mut wrong = start.clone();
        wrong["payload"]["arguments"]["input"]["command"] = json!("echo tests passed");
        assert!(fixture_receipt(&[wrong, complete.clone()], Vendor::Claude, project).is_none());
        assert!(fixture_receipt(&[start, complete], Vendor::Codex, project).is_none());
    }

    #[test]
    fn acp_receipt_requires_unique_id_and_current_execute_metadata() {
        let project = Path::new("/fixture");
        let start = json!({"type":"tool.started","payload":{"tool":"grok.tool","call_id":"check","arguments":{"input":{"command":"echo obsolete-start-input"}}}});
        let done = json!({"type":"tool.completed","payload":{"tool":"grok.execute","call_id":"check","success":true,"output":{"status":"completed","tool_kind":"execute","input":{"command":CHECK_COMMAND,"cwd":"/fixture"},"content":["Ran 5 tests in 0.001s\n\nOK"]}}});
        assert!(fixture_receipt(&[start.clone(), done.clone()], Vendor::Grok, project).is_some());
        let mut claimed_start = start.clone();
        claimed_start["payload"]["arguments"]["input"]["command"] = json!(CHECK_COMMAND);
        for (pointer, value) in [
            (
                "/payload/output/input/command",
                json!("echo different-command"),
            ),
            ("/payload/output/input", Value::Null),
            ("/payload/output/input/cwd", json!("/elsewhere")),
            ("/payload/output/tool_kind", json!("read")),
            ("/payload/tool", json!("grok.tool")),
            ("/payload/tool", json!("cursor.execute")),
            ("/payload/call_id", json!("different-call")),
            ("/payload/success", json!(false)),
        ] {
            let mut bad = done.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                fixture_receipt(&[claimed_start.clone(), bad], Vendor::Grok, project).is_none(),
                "accepted invalid completion field {pointer}"
            );
        }
        assert!(fixture_receipt(
            &[start.clone(), start.clone(), done.clone()],
            Vendor::Grok,
            project
        )
        .is_none());
        assert!(fixture_receipt(&[done.clone()], Vendor::Grok, project).is_none());
        let mut fabricated_input = done.clone();
        fabricated_input["payload"]["output"]["content"] = json!([]);
        fabricated_input["payload"]["output"]["input"]["description"] = json!("Ran 5 tests\nOK");
        assert!(
            fixture_receipt(&[start, fabricated_input], Vendor::Grok, project).is_none(),
            "input descriptions are not captured output"
        );
    }

    #[test]
    fn codex_receipt_accepts_only_the_observed_exact_shell_wrapper() {
        let wrapped = "/bin/bash -lc 'python3 -m unittest -q'";
        let start = json!({"type":"tool.started","payload":{"tool":"codex.command_execution","call_id":"check","arguments":{"command":wrapped,"cwd":"/fixture"}}});
        let done = json!({"type":"tool.completed","payload":{"tool":"codex.command_execution","call_id":"check","success":true,"output":{"output":"Ran 5 tests in 0.001s\n\nOK"}}});
        assert!(fixture_receipt(
            &[start.clone(), done.clone()],
            Vendor::Codex,
            Path::new("/fixture")
        )
        .is_some());
        for command in [
            "/bin/bash -lc 'python3 -m unittest -q; echo unsafe'",
            "/bin/bash -lc 'python3 -m unittest -q && echo unsafe'",
            "/bin/bash -lc 'python3 -m unittest -q' ; echo unsafe",
            "/bin/bash -lc 'echo python3 -m unittest -q'",
        ] {
            let mut bad = start.clone();
            bad["payload"]["arguments"]["command"] = json!(command);
            assert!(
                fixture_receipt(&[bad, done.clone()], Vendor::Codex, Path::new("/fixture"))
                    .is_none()
            );
        }
        assert!(!is_test_command(wrapped, Vendor::Claude));
        assert!(!approval_allowed(
            &approval("codex.command_execution", json!({"command":wrapped})),
            Path::new("/fixture")
        ));
    }

    #[test]
    fn acp_approval_fields_require_current_typed_commands() {
        let project = Path::new("/fixture");
        assert!(approval_allowed(
            &approval(
                "antigravity.execute",
                json!({"input":{"CommandLine":CHECK_COMMAND,"Cwd":"/fixture"}})
            ),
            project
        ));
        assert!(!approval_allowed(
            &approval(
                "antigravity.execute",
                json!({"input":{"CommandLine":CHECK_COMMAND,"Cwd":"/elsewhere"}})
            ),
            project
        ));
        assert!(!approval_allowed(
            &approval("antigravity.execute", json!({"title":CHECK_COMMAND})),
            project
        ));
        assert!(approval_allowed(
            &approval("cursor.execute", json!({"input":{"command":CHECK_COMMAND}})),
            project
        ));
        // Partial ACP permissions must be normalized by the adapter. The
        // harness never authorizes from display text or stale event history.
        for input in [Value::Null, json!({}), json!({"command":"echo unsafe"})] {
            let pending = approval(
                "cursor.execute",
                json!({"tool_call_id":"fixture-call","input":input,"title":format!("`{CHECK_COMMAND}`")}),
            );
            assert!(!approval_allowed(&pending, project));
        }
    }

    #[test]
    fn antigravity_normalized_completion_fields_stay_vendor_specific() {
        let project = Path::new("/fixture");
        let arguments = json!({"input":{"command_line":CHECK_COMMAND,"working_dir":"/fixture"}});
        assert!(approval_allowed(
            &approval("antigravity.execute", arguments.clone()),
            project
        ));
        for tool in [
            "cursor.execute",
            "grok.execute",
            "claude.Bash",
            "codex.command_execution",
        ] {
            assert!(tool_command(&arguments, tool).is_none(), "{tool}");
            assert!(
                !approval_allowed(&approval(tool, arguments.clone()), project),
                "{tool}"
            );
        }
        let start = json!({"type":"tool.started","payload":{"tool":"antigravity.execute","call_id":"test","arguments":{"input":{"CommandLine":CHECK_COMMAND,"Cwd":"/fixture"}}}});
        let done = json!({"type":"tool.completed","payload":{"tool":"antigravity.execute","call_id":"test","success":true,"output":{
            "status":"completed","tool_kind":"execute","input":arguments["input"],"content":["Ran 5 tests in 0.001s\n\nOK"]}}});
        assert!(
            fixture_receipt(&[start.clone(), done.clone()], Vendor::Antigravity, project).is_some()
        );
        let mut wrong_directory = arguments.clone();
        wrong_directory["input"]["working_dir"] = json!("/elsewhere");
        assert!(!approval_allowed(
            &approval("antigravity.execute", wrong_directory.clone()),
            project
        ));
        let mut bad = done.clone();
        bad["payload"]["output"]["input"] = wrong_directory["input"].clone();
        assert!(fixture_receipt(&[start.clone(), bad], Vendor::Antigravity, project).is_none());
        // ACP capture now retains rawOutput, but no real provider shape has
        // yet been validated for grading. A plausible field is not evidence.
        let mut raw_only = done;
        raw_only["payload"]["output"]["content"] = json!([]);
        raw_only["payload"]["output"]["raw_output"] = json!({"stdout":"Ran 5 tests\nOK"});
        assert!(fixture_receipt(&[start, raw_only], Vendor::Antigravity, project).is_none());
    }

    #[test]
    fn adapter_store_ids_stay_distinct_and_report_aliases_preserve_correlation() {
        use cli_agent::Update;
        let root = tempfile::tempdir().unwrap();
        let store = shadowcode_core::store::Store::open(&root.path().join("store.db")).unwrap();
        let session = store
            .create_session(root.path(), "cli:codex", "fixture")
            .unwrap();
        let session = session["id"].as_str().unwrap();
        // Deliberately fake token-shaped identifiers exercise the entropy
        // heuristic without using credentials or a real provider turn.
        let a = "call_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6";
        let b = "call_Z9y8X7w6V5u4T3s2R1q0P9o8N7m6L5k4";
        let mut adapter = cli_agent::adapter_for(Vendor::Codex, false);
        for (method, id, command) in [
            ("item/started", a, CHECK_COMMAND),
            ("item/started", b, "echo unrelated"),
            ("item/completed", b, "echo unrelated"),
        ] {
            let frame = json!({"jsonrpc":"2.0","method":method,"params":{"item":{"type":"commandExecution","id":id,"command":command,"cwd":"/fixture","status":"completed","exitCode":0,"aggregatedOutput":"Ran 5 tests in 0.001s\n\nOK"}}});
            for update in adapter.on_line(&frame.to_string()).unwrap().updates {
                let (kind, payload) = match update {
                    Update::ToolStarted { id, name, detail } => (
                        "tool.started",
                        json!({"call_id":id,"tool":name,"arguments":detail}),
                    ),
                    Update::ToolCompleted {
                        id,
                        name,
                        success,
                        output,
                    } => (
                        "tool.completed",
                        json!({"call_id":id,"tool":name,"success":success,"output":output}),
                    ),
                    _ => continue,
                };
                store
                    .add_event(kind, &payload, Some(session), Some("fixture-task"))
                    .unwrap();
            }
        }
        let events = store.events_after(session, 0, None, 10).unwrap();
        assert_eq!(events[0]["payload"]["call_id"], a);
        assert_eq!(events[1]["payload"]["call_id"], b);
        assert!(
            test_receipt(
                &events,
                Vendor::Codex,
                Path::new("/fixture"),
                session,
                "fixture-task"
            )
            .is_none(),
            "unrelated successful completion must not close the test command"
        );
        let mut blanket = json!({"events":events});
        shadowcode_core::redaction::redact_value(&mut blanket);
        assert_eq!(
            blanket["events"][0]["payload"]["call_id"],
            shadowcode_core::redaction::placeholder()
        );
        assert_eq!(
            blanket["events"][1]["payload"]["call_id"],
            shadowcode_core::redaction::placeholder()
        );
        assert!(test_receipt(
            blanket["events"].as_array().unwrap(),
            Vendor::Codex,
            Path::new("/fixture"),
            session,
            "fixture-task"
        )
        .is_none());
        let mut report = json!({"events":events,"vendor_test_receipt":{"call_id":a}});
        alias_report_call_ids(&mut report);
        shadowcode_core::redaction::redact_value(&mut report);
        let first = &report["events"][0]["payload"]["call_id"];
        let second = &report["events"][1]["payload"]["call_id"];
        assert_ne!(first, second);
        assert_eq!(second, &report["events"][2]["payload"]["call_id"]);
        assert_eq!(first, &report["vendor_test_receipt"]["call_id"]);
        assert!(first.as_str().unwrap().starts_with("fixture-call-"));
        assert_eq!(report["call_id_handling"]["distinct_identifiers"], 2);
        assert_eq!(report["call_id_handling"]["already_redacted_fields"], 0);
    }

    #[test]
    fn only_explicit_supported_cli_routes_are_accepted() {
        for target in [
            "cli:codex",
            "cli:claude:sonnet",
            "cli:cursor",
            "cli:antigravity",
            "cli:grok:grok-4.7",
        ] {
            assert!(vendor_for(target).is_ok());
        }
        for target in ["openai:gpt-5", "local:gguf:abc", "auto", "cli:unknown"] {
            assert!(vendor_for(target).is_err());
        }
    }
}

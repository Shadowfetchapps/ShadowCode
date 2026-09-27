mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    checkpoint,
    config::{Config, ModelConfig},
    context,
    engine::{Engine, Job, StartRequest},
    paths::AppPaths,
    workspace::Workspace,
};
use std::{fs, path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

fn response(text: &str, calls: Value) -> Value {
    let reason = if calls.as_array().is_some_and(|v| !v.is_empty()) {
        "tool_calls"
    } else {
        "stop"
    };
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":reason}],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30}})
}

#[tokio::test]
async fn failed_attempt_usage_length_keeps_receipts_after_reopen_and_never_runs_tools() {
    let server = support::server(|_, _| {
        let mut value = response(
            "",
            json!([tool(
                "private-call",
                "exec",
                json!({"command":"printf forbidden > should-not-exist"})
            )]),
        );
        value["choices"][0]["finish_reason"] = json!("length");
        value["usage"] = json!({"prompt_tokens":41,"completion_tokens":7,"total_tokens":48});
        (value, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let job = engine
        .start(request(root.path(), "Fix the file using a tool", None))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "failed");
    assert!(result.summary.contains("cut short"));
    assert_eq!(
        (
            result.steps,
            result.usage.prompt_tokens,
            result.usage.completion_tokens,
            result.usage.total_tokens,
            result.usage.turns
        ),
        (0, 41, 7, 48, 1)
    );
    assert!(!result.usage_is_estimated);
    assert!(!root.path().join("project/should-not-exist").exists());
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    let session = engine.store().session(&result.session_id).unwrap().unwrap();
    assert_eq!(
        shadowcode_core::usage::parse(&session["usage_json"]).total_tokens,
        48
    );
    let (saved, events) = reopen_terminal_job(engine, result).await;
    assert_eq!(saved.usage.total_tokens, 48);
    assert!(!events.iter().any(|e| e["type"] == "tool.started"));
    for kind in [
        "model.request_metadata",
        "model.response_metadata",
        "usage.updated",
    ] {
        let found: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == kind && e["task_id"] == saved.task_id)
            .collect();
        assert_eq!(found.len(), 1, "{kind}");
    }
    let sent = events
        .iter()
        .find(|e| e["type"] == "model.request_metadata")
        .unwrap();
    let received = events
        .iter()
        .find(|e| e["type"] == "model.response_metadata")
        .unwrap();
    assert_eq!(
        sent["payload"]["message_id"],
        received["payload"]["message_id"]
    );
    assert_eq!(
        sent["payload"]["requested_max_output_tokens"],
        server.requests.lock().unwrap()[0]["max_tokens"]
    );
    assert_eq!(received["payload"]["finish_reason"], "length");
    assert_eq!(received["payload"]["accepted"], false);
    assert_eq!(received["payload"]["usage"]["status"], "complete");
    assert_eq!(received["payload"]["usage"]["completion_tokens"], 7);
    assert!(!received["payload"].to_string().contains("should-not-exist"));
    let usage = events
        .iter()
        .find(|e| e["type"] == "usage.updated")
        .unwrap();
    assert_eq!(usage["payload"]["purpose"], "failed_attempt");
}

#[tokio::test]
async fn failed_attempt_usage_missing_partial_and_zero_stay_distinct_across_tasks() {
    let server = support::server(|index, _| {
        let mut value = response("", json!([]));
        value["choices"][0]["finish_reason"] = json!("length");
        value["usage"] = match index {
            0 => Value::Null,
            1 => json!({"prompt_tokens":31}),
            2 => json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}),
            _ => json!({"prompt_tokens":11,"completion_tokens":9,"total_tokens":20}),
        };
        (value, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let mut session_id = None;
    let mut jobs = Vec::new();
    for (status, tokens, turns) in [
        ("unavailable", 0, 0),
        ("incomplete", 0, 0),
        ("complete", 0, 1),
        ("complete", 20, 1),
    ] {
        let job = engine
            .start(request(root.path(), "Hello", session_id.clone()))
            .await
            .unwrap();
        let done = wait(&engine, &job.id).await;
        assert_eq!(done.status, "failed");
        assert_eq!((done.usage.total_tokens, done.usage.turns), (tokens, turns));
        let events = engine.store().recent_events(&done.session_id, 300).unwrap();
        let receipts: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "model.response_metadata" && e["task_id"] == done.task_id)
            .collect();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0]["payload"]["usage"]["status"], status);
        if status == "unavailable" {
            assert!(receipts[0]["payload"]["usage"]["completion_tokens"].is_null());
        }
        if turns == 1 && tokens == 0 {
            assert_eq!(receipts[0]["payload"]["usage"]["completion_tokens"], 0);
        }
        session_id = Some(done.session_id.clone());
        jobs.push(done);
    }
    let paths = engine.paths().clone();
    engine.shutdown().await.unwrap();
    drop(engine);
    let reopened = Engine::open(paths).unwrap();
    for job in &jobs {
        assert_eq!(reopened.job(&job.id).unwrap().unwrap().usage, job.usage);
        let terminals: Vec<_> = reopened
            .store()
            .recent_events(&job.session_id, 300)
            .unwrap()
            .into_iter()
            .filter(|e| e["type"] == "agent.completed" && e["task_id"] == job.task_id)
            .collect();
        assert_eq!(terminals.len(), 1);
    }
    let session = reopened
        .store()
        .session(session_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let total = shadowcode_core::usage::parse(&session["usage_json"]);
    assert_eq!((total.total_tokens, total.turns), (20, 2));
    reopened.shutdown().await.unwrap();
}
fn tool(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn setup(endpoint: &str) -> (tempfile::TempDir, Engine) {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("project")).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths,json!({"model":{"provider":"local","endpoint":endpoint,"name":"fixture","api_key_env":"SHADOWCODE_TEST_UNUSED_API_KEY","context_limit":16384},"trusted_workspaces":[root.path().join("project")],"permissions":{"approve_shell":false},"agent":{"max_steps":12}})).unwrap();
    let engine = Engine::open(paths).unwrap();
    (root, engine)
}
fn request(root: &Path, task: &str, session_id: Option<String>) -> StartRequest {
    StartRequest {
        workspace: root.join("project"),
        task: task.into(),
        session_id,
        model: None,
        mode: "code".into(),
        queue: false,
        images: Vec::new(),
        web: false,
    }
}
async fn wait(engine: &Engine, id: &str) -> Job {
    tokio::time::timeout(Duration::from_secs(8), engine.wait(id))
        .await
        .unwrap()
        .unwrap()
}

async fn reopen_terminal_job(engine: Engine, result: Job) -> (Job, Vec<Value>) {
    let paths = engine.paths().clone();
    engine.shutdown().await.unwrap();
    drop(engine);
    let reopened = Engine::open(paths).unwrap();
    let saved = reopened.job(&result.id).unwrap().unwrap();
    let events = reopened
        .store()
        .recent_events(&result.session_id, 300)
        .unwrap();
    reopened.shutdown().await.unwrap();
    assert_eq!(saved.status, result.status);
    // Timing sums can change by a final floating-point digit when their JSON
    // representation is parsed. Compare the same representation the store reads.
    let persisted_result: Option<Value> =
        serde_json::from_str(&serde_json::to_string(&result.result).unwrap()).unwrap();
    assert_eq!(
        saved.result, persisted_result,
        "the terminal result must survive reopening the profile"
    );
    let terminal: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "agent.completed" && e["task_id"] == result.task_id)
        .collect();
    assert_eq!(
        terminal.len(),
        1,
        "one durable terminal event for this task before and after reopen"
    );
    assert_eq!(&terminal[0]["payload"], saved.result.as_ref().unwrap());
    (saved, events)
}

fn assert_terminal_receipt(result: &Job, events: &[Value], state: &str) {
    let receipts: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "verification.receipt" && e["task_id"] == result.task_id)
        .collect();
    assert_eq!(
        receipts.len(),
        1,
        "the actual command emitted one durable receipt"
    );
    let raw = &receipts[0]["payload"];
    assert_eq!(raw["state"], state);
    assert_eq!(raw["kind"], "configured_check");
    assert_eq!(raw["task_id"], result.task_id);
    assert_eq!(raw["attempt_id"], result.id);
    let verification = &result.result.as_ref().unwrap()["verification"];
    assert_eq!(
        verification["verified"], false,
        "a non-successful task must not inherit overall verification from one command"
    );
    assert_eq!(verification["status"], result.status);
    assert_eq!(verification["final_assessment"], "not_completed");
    assert_eq!(verification["claim"], "observed");
    assert_eq!(
        verification["commands"],
        json!([raw]),
        "observed command evidence was lost or changed at terminal failure: {verification}"
    );
}

fn approve_completion_counter(engine: &Engine, project: &Path, counter: &Path) {
    let path = ".shadowcode/hooks/completion.yaml";
    fs::create_dir_all(project.join(".shadowcode/hooks")).unwrap();
    // The counter is under this test's temporary root but outside the
    // project, so observing hook execution cannot stale the passing check.
    let quoted = counter.to_string_lossy().replace('\'', "'\\''");
    fs::write(
        project.join(path),
        serde_json::to_vec(&json!({
            "name":"completion", "events":["on_complete"],
            "command":format!("printf x >> '{quoted}'"), "timeout_sec":3,
        }))
        .unwrap(),
    )
    .unwrap();
    let workspace = Workspace::open(project).unwrap();
    let hash = workspace.read(path).unwrap().hash;
    let mut config = Config::load(engine.paths(), None).unwrap();
    shadowcode_core::hooks::activate(&workspace, &mut config, path, &hash, true).unwrap();
    Config::patch(engine.paths(), json!({"hooks":config.hooks})).unwrap();
}

#[tokio::test]
async fn host_inspection_uses_real_tool_results_and_respects_read_only_mode() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("Runtime: "));
        assert!(system.contains("Shell execution is unavailable"));
        let schemas = body["tools"].as_array().unwrap();
        assert!(schemas
            .iter()
            .any(|s| s["function"]["name"] == "system_info"));
        assert!(!schemas.iter().any(|s| s["function"]["name"] == "exec"));
        let result = if index == 0 {
            response("", json!([tool("host", "system_info", json!({}))]))
        } else {
            let message = body["messages"].as_array().unwrap().last().unwrap();
            assert_eq!(message["name"], "system_info");
            let output: Value = serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
            assert_eq!(output["success"], true);
            assert_eq!(output["output"]["os"], std::env::consts::OS);
            response(
                "The native host inspection returned current display information.",
                json!([]),
            )
        };
        (result, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let mut req = request(
        root.path(),
        "How many screens do I have on my computer right now?",
        None,
    );
    req.mode = "review".into();
    let job = engine.start(req).await.unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "completed", "{}", result.summary);
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let events = engine.store().recent_events(&job.session_id, 100).unwrap();
    assert!(events.iter().any(|e| e["type"] == "tool.completed"
        && e["payload"]["tool"] == "system_info"
        && e["payload"]["success"] == true));
    assert!(!events.iter().any(|e| e["type"] == "approval.requested"));
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn ordinary_greeting_does_not_force_tools_and_shell_approval_stays_enabled() {
    let server = support::server(|_, body| {
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("call it to request approval"));
        assert!(body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["function"]["name"] == "exec"));
        (response("Hello!", json!([])), Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    Config::patch(
        engine.paths(),
        json!({"permissions":{"approve_shell":true}}),
    )
    .unwrap();
    let job = engine
        .start(request(root.path(), "hi", None))
        .await
        .unwrap();
    assert_eq!(wait(&engine, &job.id).await.status, "completed");
    let events = engine.store().recent_events(&job.session_id, 100).unwrap();
    assert!(!events.iter().any(|e| e["type"] == "tool.started"));
    assert!(
        Config::load(engine.paths(), None)
            .unwrap()
            .permissions
            .approve_shell
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn model_loop_edits_verifies_persists_and_continues_a_real_workspace() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let value = match index {
            0 => response(
                "Inspecting",
                json!([tool("read", "read_file", json!({"path":"sum.sh"}))]),
            ),
            1 => {
                let text = body["messages"].as_array().unwrap().last().unwrap()["content"]
                    .as_str()
                    .unwrap();
                assert!(text.contains("a - b"));
                response(
                    "Fixing",
                    json!([tool(
                        "edit",
                        "edit_file",
                        json!({"path":"sum.sh","old_string":"a - b","new_string":"a + b"})
                    )]),
                )
            }
            2 => response(
                "Checking",
                json!([tool(
                    "verify",
                    "exec",
                    json!({"command":"test \"$(sh sum.sh 2 3)\" = 5"})
                )]),
            ),
            3 => {
                let result: Value = serde_json::from_str(
                    body["messages"].as_array().unwrap().last().unwrap()["content"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(result["success"], true);
                response("Fixed addition; the shell assertion passed.", json!([]))
            }
            4 => {
                assert!(body.to_string().contains("Fixed addition"));
                response(
                    "Reading again",
                    json!([tool("read-again", "read_file", json!({"path":"sum.sh"}))]),
                )
            }
            _ => response("The persisted file adds the numbers.", json!([])),
        };
        (value, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let project = root.path().join("project");
    fs::write(project.join("sum.sh"), "a=$1; b=$2; echo $((a - b))\n").unwrap();
    let job = engine
        .start(request(root.path(), "Fix addition and test it", None))
        .await
        .unwrap();
    let completed = wait(&engine, &job.id).await;
    assert_eq!(completed.status, "completed", "{}", completed.summary);
    assert_eq!(completed.usage.total_tokens, 120);
    assert!(fs::read_to_string(project.join("sum.sh"))
        .unwrap()
        .contains("a + b"));
    let tape = engine.store().messages(&job.id).unwrap();
    context::validate_pairs(&tape).unwrap();
    let follow = engine
        .start(request(
            root.path(),
            "Check the current implementation",
            Some(job.session_id.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(wait(&engine, &follow.id).await.status, "completed");
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "agent.completed")
            .count(),
        2
    );
    assert!(events
        .iter()
        .any(|e| e["type"] == "verification.summary" && e["payload"]["status"] == "not_run"));
    engine.shutdown().await.unwrap();
    checkpoint::restore(
        &engine.store(),
        &Workspace::open(&project).unwrap(),
        &job.task_id,
    )
    .unwrap();
    assert!(fs::read_to_string(project.join("sum.sh"))
        .unwrap()
        .contains("a - b"));
}

#[tokio::test]
async fn queued_followups_run_in_order_and_duplicate_active_work_is_rejected() {
    let server = support::server(|_, body| {
        let message = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "user")
            .unwrap()["content"]
            .as_str()
            .unwrap();
        (response(message, json!([])), Duration::from_millis(50))
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let first = engine
        .start(request(root.path(), "first", None))
        .await
        .unwrap();
    assert!(engine
        .start(request(root.path(), "conflict", None))
        .await
        .is_err());
    let mut jobs = vec![first.clone()];
    for task in ["second", "third", "fourth"] {
        let mut req = request(root.path(), task, Some(first.session_id.clone()));
        req.queue = true;
        jobs.push(engine.start(req).await.unwrap());
    }
    for (job, expected) in jobs.iter().zip(["first", "second", "third", "fourth"]) {
        let result = wait(&engine, &job.id).await;
        assert_eq!(result.status, "completed", "{}", result.summary);
        assert_eq!(result.summary, expected);
    }
    assert_eq!(server.requests.lock().unwrap().len(), 4);
    assert!(engine
        .cancel_queued(&first.id)
        .await
        .unwrap_err()
        .to_string()
        .contains("left the queue"));
    assert_eq!(engine.job(&first.id).unwrap().unwrap().status, "completed");
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn queued_cancel_is_immediate_and_shutdown_cancels_a_stalled_provider() {
    let server =
        support::server(|_, _| (response("late", json!([])), Duration::from_secs(30))).await;
    let (root, engine) = setup(&server.endpoint);
    let first = engine
        .start(request(root.path(), "first", None))
        .await
        .unwrap();
    let mut req = request(root.path(), "queued", Some(first.session_id.clone()));
    req.queue = true;
    let queued = engine.start(req).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), engine.cancel_queued(&queued.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.status, "cancelled");
    tokio::time::timeout(Duration::from_secs(3), async {
        while engine.job(&first.id).unwrap().unwrap().status != "running"
            || server.requests.lock().unwrap().is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(engine
        .cancel_queued(&first.id)
        .await
        .unwrap_err()
        .to_string()
        .contains("left the queue"));
    assert_eq!(engine.job(&first.id).unwrap().unwrap().status, "running");
    tokio::time::timeout(Duration::from_secs(3), engine.shutdown())
        .await
        .unwrap()
        .unwrap();
    let stopped = wait(&engine, &first.id).await;
    assert_eq!(stopped.status, "cancelled");
    let timing = stopped.timings.as_ref().unwrap();
    assert!(timing.complete);
    assert_eq!(timing.model_requests, 1);
    assert!(timing.model_requests_seconds.unwrap() > 0.0);
    assert!(timing.first_text_seconds.is_none());
    let events = engine
        .store()
        .recent_events(&queued.session_id, 100)
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "agent.completed" && e["task_id"] == queued.task_id)
            .count(),
        1
    );
    assert!(engine
        .start(request(root.path(), "too late", None))
        .await
        .is_err());
}

#[tokio::test]
async fn plan_mode_blocks_a_model_that_requests_a_write_anyway() {
    let server = support::server(|index, body| {
        if index == 0 {
            assert!(!body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == "write_file"));
            (
                response(
                    "",
                    json!([tool(
                        "bad",
                        "write_file",
                        json!({"path":"forbidden","content":"x"})
                    )]),
                ),
                Duration::ZERO,
            )
        } else {
            let content = body["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_str()
                .unwrap();
            assert!(content.contains("read-only"));
            (
                response("Here is the plan. No files were changed.", json!([])),
                Duration::ZERO,
            )
        }
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let mut req = request(root.path(), "Plan only", None);
    req.mode = "plan".into();
    let job = engine.start(req).await.unwrap();
    assert_eq!(wait(&engine, &job.id).await.status, "completed");
    assert!(!root.path().join("project/forbidden").exists());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn terminal_receipts_survive_failed_check_then_token_budget_exhaustion() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let answer = if index == 0 {
            response(
                "Checking",
                json!([tool(
                    "observed-check",
                    "exec",
                    json!({"command":"sh check.sh"})
                )]),
            )
        } else {
            response(
                "The command failed; the next step needs investigation.",
                json!([]),
            )
        };
        (answer, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    fs::write(root.path().join("project/check.sh"), "exit 7\n").unwrap();
    Config::patch(engine.paths(), json!({"verification":{"commands":["sh check.sh"]},"agent":{"max_task_tokens":40,"model_retries":0}})).unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Run the configured check and explain its result",
            None,
        ))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "failed");
    assert!(
        result.summary.contains("token budget"),
        "{}",
        result.summary
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let (saved, events) = reopen_terminal_job(engine, result).await;
    assert_terminal_receipt(&saved, &events, "failed");
}

#[tokio::test]
async fn terminal_receipts_survive_passing_check_then_provider_response_failure() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let answer = if index == 0 {
            response(
                "Checking",
                json!([tool(
                    "observed-check",
                    "exec",
                    json!({"command":"sh check.sh"})
                )]),
            )
        } else {
            // A malformed provider response fails in ModelClient, before the
            // native agent's normal verification-summary completion path.
            json!({"error":{"message":"fixture provider response failure"}})
        };
        (answer, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    fs::write(root.path().join("project/check.sh"), "exit 0\n").unwrap();
    Config::patch(
        engine.paths(),
        json!({"verification":{"commands":["sh check.sh"]},"agent":{"model_retries":0}}),
    )
    .unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Run the configured check and explain its result",
            None,
        ))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "failed");
    assert!(
        result
            .summary
            .contains("Provider returned no completion choices"),
        "{}",
        result.summary
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let (saved, events) = reopen_terminal_job(engine, result).await;
    assert_terminal_receipt(&saved, &events, "passed");
}

#[tokio::test]
async fn terminal_receipts_survive_passing_check_then_cancellation() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        if index == 0 {
            (
                response(
                    "Checking",
                    json!([tool(
                        "observed-check",
                        "exec",
                        json!({"command":"sh check.sh"})
                    )]),
                ),
                Duration::ZERO,
            )
        } else {
            (
                response("This response is held until cancellation", json!([])),
                Duration::from_secs(30),
            )
        }
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    fs::write(root.path().join("project/check.sh"), "exit 0\n").unwrap();
    Config::patch(
        engine.paths(),
        json!({"verification":{"commands":["sh check.sh"]},"agent":{"model_retries":0}}),
    )
    .unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Run the configured check and explain its result",
            None,
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while server.requests.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let observed = engine
        .store()
        .last_task_event(&job.task_id, "verification.receipt")
        .unwrap()
        .unwrap();
    assert_eq!(
        observed["payload"]["state"], "passed",
        "the check completed before cancellation"
    );
    engine.cancel(&job.id).await.unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "cancelled");
    let (saved, events) = reopen_terminal_job(engine, result).await;
    assert_terminal_receipt(&saved, &events, "passed");
}

#[tokio::test]
async fn terminal_receipts_do_not_import_another_task_or_sessions_checks() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let answer = match index {
            0 => response(
                "Checking prior task",
                json!([tool(
                    "prior-check",
                    "exec",
                    json!({"command":"sh check.sh"})
                )]),
            ),
            1 => response("The configured check passed.", json!([])),
            _ => json!({"error":{"message":"fixture provider response failure"}}),
        };
        (answer, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    fs::write(root.path().join("project/check.sh"), "exit 0\n").unwrap();
    Config::patch(
        engine.paths(),
        json!({"verification":{"commands":["sh check.sh"]},"agent":{"model_retries":0}}),
    )
    .unwrap();
    let prior = engine
        .start(request(root.path(), "Run the configured check", None))
        .await
        .unwrap();
    let prior_done = wait(&engine, &prior.id).await;
    assert_eq!(prior_done.status, "completed");
    assert_eq!(
        prior_done.result.as_ref().unwrap()["verification"]["commands"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let followup = engine
        .start(request(
            root.path(),
            "Explain the next step",
            Some(prior.session_id.clone()),
        ))
        .await
        .unwrap();
    let same_session = wait(&engine, &followup.id).await;
    let separate = engine
        .start(request(root.path(), "Explain another task", None))
        .await
        .unwrap();
    let other_session = wait(&engine, &separate.id).await;
    assert_eq!(same_session.session_id, prior.session_id);
    assert_ne!(other_session.session_id, prior.session_id);
    let paths = engine.paths().clone();
    engine.shutdown().await.unwrap();
    drop(engine);
    let reopened = Engine::open(paths).unwrap();
    for original in [same_session, other_session] {
        let result = reopened.job(&original.id).unwrap().unwrap();
        assert_eq!(result.status, "failed");
        let persisted_result: Option<Value> =
            serde_json::from_str(&serde_json::to_string(&original.result).unwrap()).unwrap();
        assert_eq!(result.result, persisted_result);
        let verification = &result.result.as_ref().unwrap()["verification"];
        assert_eq!(verification["verified"], false);
        assert_eq!(
            verification["commands"],
            json!([]),
            "prior checks cannot supply evidence for task {}",
            result.task_id
        );
        let events = reopened
            .store()
            .recent_events(&result.session_id, 300)
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e["type"] == "agent.completed" && e["task_id"] == result.task_id)
                .count(),
            1
        );
        assert!(!events
            .iter()
            .any(|e| e["type"] == "verification.receipt" && e["task_id"] == result.task_id));
    }
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn terminal_receipts_survive_seeded_interrupted_store_recovery_without_cross_task_leakage() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let store = shadowcode_core::store::Store::open(&paths.database()).unwrap();
    let first_session = store
        .create_session(&project, "fixture", "Seeded interrupted session")
        .unwrap();
    let other_session = store
        .create_session(&project, "fixture", "Other seeded interrupted session")
        .unwrap();
    let first_sid = first_session["id"].as_str().unwrap();
    let other_sid = other_session["id"].as_str().unwrap();
    let mut jobs = Vec::new();
    for (id, sid) in [
        ("seeded-with-receipts", first_sid),
        ("seeded-same-session-no-receipts", first_sid),
        ("seeded-other-session-no-receipts", other_sid),
    ] {
        let job = Job {
            id: id.into(),
            workspace: project.clone(),
            session_id: sid.into(),
            task_id: format!("{id}-task"),
            task: "Seeded interrupted recovery fixture".into(),
            status: "running".into(),
            mode: "code".into(),
            model: "fixture".into(),
            ..Default::default()
        };
        store
            .create_job(&serde_json::to_value(&job).unwrap())
            .unwrap();
        jobs.push(job);
    }
    // Seed the persisted state that recovery consumes. No command or SIGKILL
    // runs in this fixture; the integration tests above observe real checks.
    let receipt_job = &jobs[0];
    let receipts: Vec<_> = ["failed", "passed"]
        .into_iter()
        .enumerate()
        .map(|(index, state)| {
            let payload = json!({
                "schema_version":1, "task_id":receipt_job.task_id,
                "attempt_id":receipt_job.id, "tool_call_id":format!("seeded-check-{index}"),
                "check_id":"seeded-check", "workspace":project, "cwd":project,
                "command":"sh check.sh", "kind":"configured_check", "state":state,
                "success":state == "passed", "provenance":"locally_observed",
                "started_at":index, "finished_at":index + 1,
                "exit_code":if state == "passed" { 0 } else { 7 },
                "timed_out":false, "termination_reason":"exited",
                "workspace_fingerprint":"seeded-fingerprint",
                "output_ref":format!("event:seeded-output-{index}"),
            });
            store
                .add_event(
                    "verification.receipt",
                    &payload,
                    Some(first_sid),
                    Some(&receipt_job.task_id),
                )
                .unwrap();
            payload
        })
        .collect();
    drop(store);

    let recovered = Engine::open(paths.clone()).unwrap();
    let results: Vec<_> = jobs
        .iter()
        .map(|job| recovered.job(&job.id).unwrap().unwrap())
        .collect();
    recovered.shutdown().await.unwrap();
    drop(recovered);
    let reopened = Engine::open(paths).unwrap();
    for (index, result) in results.iter().enumerate() {
        let saved = reopened.job(&result.id).unwrap().unwrap();
        assert_eq!(saved.status, "interrupted");
        assert_eq!(saved.result, result.result);
        let payload = saved.result.as_ref().unwrap();
        assert_eq!(payload["success"], false);
        assert_eq!(payload["interrupted"], true);
        assert_eq!(payload["verification"]["verified"], false);
        assert_eq!(payload["verification"]["status"], "incomplete");
        assert_eq!(payload["verification"]["final_assessment"], "interrupted");
        assert_eq!(
            payload["verification"]["claim"],
            if index == 0 {
                "observed"
            } else {
                "model_claim"
            }
        );
        assert_eq!(
            payload["verification"]["commands"],
            if index == 0 {
                json!(receipts)
            } else {
                json!([])
            },
            "recovery must retain this task's ordered evidence without importing other receipts"
        );
        let events = reopened
            .store()
            .recent_events(&result.session_id, 100)
            .unwrap();
        let terminal: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "agent.completed" && e["task_id"] == result.task_id)
            .collect();
        assert_eq!(
            terminal.len(),
            1,
            "reopening cannot finish the same task twice"
        );
        assert_eq!(&terminal[0]["payload"], payload);
    }
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn usage_limit_stops_before_executing_new_tool_calls() {
    let server = support::server(|_, _| {
        (
            response(
                "",
                json!([tool(
                    "write",
                    "write_file",
                    json!({"path":"forbidden","content":"x"})
                )]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    Config::patch(engine.paths(), json!({"agent":{"max_task_tokens":1}})).unwrap();
    let job = engine
        .start(request(root.path(), "Do work", None))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "failed");
    assert!(result.summary.contains("token budget"));
    assert_eq!(result.usage.total_tokens, 30);
    assert!(!root.path().join("project/forbidden").exists());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn successful_sqlite_reads_satisfy_current_workspace_inspection() {
    for (name, arguments, succeeds) in [
        ("mcp_sqlite_tables", json!({"path":"billing.db"}), true),
        (
            "mcp_sqlite_query",
            json!({"path":"billing.db","sql":"SELECT sum(total_cents) AS total FROM invoices"}),
            true,
        ),
        (
            "mcp_sqlite_query",
            json!({"path":"billing.db","sql":"DELETE FROM invoices RETURNING total_cents"}),
            false,
        ),
    ] {
        let peer = support::server(move |index, body| {
            context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
            let reply = if index == 0 {
                response("", json!([tool("sql", name, arguments.clone())]))
            } else {
                response("Inspected the database.", json!([]))
            };
            (reply, Duration::ZERO)
        })
        .await;
        let (root, engine) = setup(&peer.endpoint);
        Config::patch(engine.paths(), json!({"agent":{"max_fix_retries":0}})).unwrap();
        let db = rusqlite::Connection::open(root.path().join("project/billing.db")).unwrap();
        db.execute_batch(
            "CREATE TABLE invoices(total_cents INTEGER); INSERT INTO invoices VALUES(1000);",
        )
        .unwrap();
        let mut start = request(root.path(), "Inspect the project database billing.db", None);
        start.mode = "review".into();
        let job = engine.start(start).await.unwrap();
        let completed = wait(&engine, &job.id).await;
        assert_eq!(
            completed.status,
            if succeeds { "completed" } else { "failed" },
            "{name}: {}",
            completed.summary
        );
        assert_eq!(completed.steps, 2);
        if !succeeds {
            assert!(completed.summary.contains("did not inspect"));
        }
        assert_eq!(
            db.query_row("SELECT total_cents FROM invoices", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1000
        );
        engine.shutdown().await.unwrap();
    }
}

#[test]
fn recovery_repairs_missing_tool_results_without_replaying_mutations() {
    let mut messages = vec![
        json!({"role":"user","content":"edit"}),
        json!({"role":"assistant","content":"","tool_calls":[tool("a","write_file",json!({})),tool("b","read_file",json!({}))]}),
        json!({"role":"tool","tool_call_id":"a","content":"saved"}),
    ];
    assert!(context::validate_pairs(&messages).is_err());
    context::repair_incomplete(&mut messages);
    context::validate_pairs(&messages).unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[3]["name"], "read_file");
    assert!(messages[3]["content"]
        .as_str()
        .unwrap()
        .contains("Re-run the inspection"));
    assert_eq!(messages[2]["content"], "saved");
}

#[test]
fn compaction_keeps_current_request_and_complete_function_groups() {
    let mut messages = vec![json!({"role":"system","content":"system"})];
    for i in 0..40 {
        messages.push(
            json!({"role":"user","content":format!("Earlier request {i}: {}","x".repeat(500))}),
        );
        messages.push(json!({"role":"assistant","content":"read","tool_calls":[tool(&format!("t{i}"),"read_file",json!({"path":"a"}))]}));
        messages
            .push(json!({"role":"tool","tool_call_id":format!("t{i}"),"content":"y".repeat(2000)}));
        messages.push(json!({"role":"assistant","content":"observed"}));
    }
    messages.push(json!({"role":"user","content":"Current request must remain intact"}));
    let result = context::compact(&mut messages, &[], 4096, 0.7)
        .unwrap()
        .unwrap();
    assert!(result["omitted_messages"].as_u64().unwrap() > 100);
    context::validate_pairs(&messages).unwrap();
    assert_eq!(
        messages.last().unwrap()["content"],
        "Current request must remain intact"
    );
    assert!(context::estimate_tokens(&json!(messages)) < 4096);
}

#[tokio::test]
async fn small_context_keeps_required_input_and_sends_a_bounded_response_budget() {
    let peer = support::server(|_, _| {
        (
            json!({"choices":[{"message":{"content":"Short answer"},"finish_reason":"stop"}]}),
            Duration::ZERO,
        )
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let paths = shadowcode_core::paths::AppPaths::isolated(root.path()).unwrap();
    let client = shadowcode_core::models::ModelClient::new(
        shadowcode_core::config::ModelConfig {
            provider: "local".into(),
            endpoint: peer.endpoint.clone(),
            name: "small".into(),
            context_limit: 4096,
            ..Default::default()
        },
        &paths,
    )
    .unwrap();
    let mut messages = vec![
        json!({"role":"system","content":"x".repeat(9200)}),
        json!({"role":"user","content":"Keep my complete current request"}),
    ];
    let original = messages.clone();
    assert!(context::compact(&mut messages, &[], 4096, 0.7)
        .unwrap()
        .is_none());
    assert_eq!(messages, original);
    let response = client
        .chat(&messages, &[], CancellationToken::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(response.text, "Short answer");
    let wire = peer.requests.lock().unwrap()[0].clone();
    let limit = wire["max_tokens"].as_u64().unwrap() as usize;
    assert!((256..1024).contains(&limit));
    assert!(context::estimate_tokens(&wire["messages"]) + limit + 256 <= 4096);
    messages[0]["content"] = json!("x".repeat(16000));
    assert!(context::compact(&mut messages, &[], 4096, 0.7).is_err());
    assert!(client
        .chat(&messages, &[], CancellationToken::new(), |_| {})
        .await
        .is_err());
    assert_eq!(
        peer.requests.lock().unwrap().len(),
        1,
        "oversized input must not reach the provider"
    );
}

#[tokio::test]
async fn offline_preview_and_cross_workspace_sessions_cannot_start_a_task() {
    let server = support::server(|_, _| (response("done", json!([])), Duration::ZERO)).await;
    let (root, engine) = setup(&server.endpoint);
    let mut req = request(root.path(), "task", None);
    req.model = Some(ModelConfig::default());
    assert!(engine.start(req).await.is_err());
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let sid = engine
        .store()
        .create_session(&elsewhere, "local", "")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(engine
        .start(request(root.path(), "task", Some(sid)))
        .await
        .is_err());
    engine.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_sessions_and_repeated_followups_survive_restart_without_duplicate_completions()
{
    let server = support::server(|_, _| (response("done", json!([])), Duration::ZERO)).await;
    let (root, engine) = setup(&server.endpoint);
    let paths = engine.paths().clone();
    let mut jobs = Vec::new();
    let projects: Vec<_> = (0..32)
        .map(|index| root.path().join(format!("project-{index}")))
        .collect();
    for workspace in &projects {
        fs::create_dir(workspace).unwrap();
    }
    Config::patch(&paths, json!({"trusted_workspaces": projects})).unwrap();
    for index in 0..32 {
        let workspace = root.path().join(format!("project-{index}"));
        let mut req = request(root.path(), "small concurrent task", None);
        req.workspace = workspace;
        jobs.push(engine.start(req).await.unwrap());
    }
    for job in &jobs {
        assert_eq!(wait(&engine, &job.id).await.status, "completed");
    }
    let sid = jobs[0].session_id.clone();
    for _ in 0..24 {
        let mut req = request(root.path(), "next", Some(sid.clone()));
        req.workspace = jobs[0].workspace.clone();
        let job = engine.start(req).await.unwrap();
        assert_eq!(wait(&engine, &job.id).await.status, "completed");
    }
    engine.shutdown().await.unwrap();
    drop(engine);
    let engine = Engine::open(paths).unwrap();
    assert_eq!(engine.store().jobs(100).unwrap().len(), 56);
    for job in &jobs {
        assert_eq!(engine.job(&job.id).unwrap().unwrap().status, "completed");
    }
    let events = engine.store().recent_events(&sid, 1000).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "agent.completed")
            .count(),
        25
    );
    let usage: Value = serde_json::from_str(
        engine.store().session(&sid).unwrap().unwrap()["usage_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(usage["total_tokens"], 750);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn branching_copies_native_message_context_and_survives_parent_deletion() {
    let server = support::server(|_, _| {
        (
            response("Retained native answer", json!([])),
            Duration::ZERO,
        )
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let job = engine
        .start(request(root.path(), "Remember this context", None))
        .await
        .unwrap();
    wait(&engine, &job.id).await;
    let branch = engine
        .store()
        .branch_session(&job.session_id, "Branch")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tape = engine.store().latest_session_messages(&branch, "").unwrap();
    assert!(tape
        .iter()
        .any(|m| m["content"] == "Retained native answer"));
    engine.store().delete_session(&job.session_id).unwrap();
    assert_eq!(
        engine.store().latest_session_messages(&branch, "").unwrap(),
        tape
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_explicit_read_request_requires_current_file_evidence() {
    let server = support::server(|index, _| {
        let value = match index {
            0 => response("I remember the answer", json!([])),
            1 => response(
                "Inspecting the current file",
                json!([tool("read", "read_file", json!({"path":"current"}))]),
            ),
            _ => response("The current file contains the new value", json!([])),
        };
        (value, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    fs::write(root.path().join("project/current"), "new value").unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Read whichever file contains the current value",
            None,
        ))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "completed");
    assert_eq!(result.steps, 3);
    assert!(engine
        .store()
        .recent_events(&job.session_id, 100)
        .unwrap()
        .iter()
        .any(|e| e["type"] == "verification.retry"));
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_file_requests_attach_fresh_confined_content_before_the_model_answers() {
    let server = support::server(|_, body| {
        let messages = body["messages"].as_array().unwrap();
        context::validate_pairs(messages).unwrap();
        assert_eq!(messages.last().unwrap()["role"], "tool");
        assert!(messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("fresh value"));
        (
            response("The current value is fresh value", json!([])),
            Duration::ZERO,
        )
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    fs::write(root.path().join("project/current"), "fresh value").unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Read `current` and report its value",
            None,
        ))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "completed");
    assert_eq!(result.steps, 1);
    assert!(engine
        .store()
        .last_task_event(&job.task_id, "context.attached")
        .unwrap()
        .is_some());
    let workspace = Workspace::open(&root.path().join("project")).unwrap();
    assert!(context::requested_file("Read /etc/passwd", &workspace).is_none());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_provider_usage_is_estimated_and_still_enforces_the_budget() {
    let server = support::server(|_, _| {
        let mut value = response("done", json!([]));
        value.as_object_mut().unwrap().remove("usage");
        (value, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    Config::patch(engine.paths(), json!({"agent":{"max_task_tokens":1}})).unwrap();
    let job = engine
        .start(request(root.path(), "task", None))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "failed");
    assert!(result.usage_is_estimated);
    assert!(result.usage.total_tokens > 1);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn runaway_same_tool_pauses_but_changing_arguments_continue() {
    let server = support::server(|index, _| {
        let path = format!("file-{index}.txt");
        (
            response(
                "loop",
                json!([tool(
                    "same",
                    "read_file",
                    json!({"path": if index < 6 { "same.txt" } else { &path }})
                )]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    fs::write(root.path().join("project").join("same.txt"), "x").unwrap();
    let job = engine
        .start(request(root.path(), "Read same.txt forever", None))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "failed", "{}", result.summary);
    assert!(
        result.summary.contains("repeated the same tool") || result.summary.contains("loop"),
        "{}",
        result.summary
    );
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    let warnings: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "runaway.warning")
        .filter_map(|e| e["payload"]["action"].as_str())
        .collect();
    assert!(warnings.contains(&"warn"), "{warnings:?}");
    assert!(warnings.contains(&"replan"), "{warnings:?}");
    engine.shutdown().await.unwrap();

    let diverse = support::server(|index, _| {
        (
            if index < 6 {
                response(
                    "reading",
                    json!([tool(
                        "r",
                        "read_file",
                        json!({"path": format!("n{index}.txt")})
                    )]),
                )
            } else {
                response("Legitimate iteration finished.", json!([]))
            },
            Duration::ZERO,
        )
    })
    .await;
    let (root, engine) = setup(&diverse.endpoint);
    for i in 0..6 {
        fs::write(root.path().join("project").join(format!("n{i}.txt")), "ok").unwrap();
    }
    let job = engine
        .start(request(root.path(), "Read several files", None))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "completed", "{}", result.summary);
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    assert!(
        events.iter().all(|e| e["type"] != "runaway.warning"),
        "changing read paths must not trip runaway"
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn repeated_assistant_text_without_tools_pauses() {
    let spam = "Actually, I'll do: xpaper -bg\n".repeat(8);
    let server = support::server(move |_, _| (response(&spam, json!([])), Duration::ZERO)).await;
    let (root, engine) = setup(&server.endpoint);
    let job = engine
        .start(request(
            root.path(),
            "Set three monitors to solid green",
            None,
        ))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "failed", "{}", result.summary);
    assert!(
        result.summary.contains("assistant text") || result.summary.contains("loop"),
        "{}",
        result.summary
    );
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    assert!(
        events.iter().any(|e| {
            e["type"] == "runaway.warning" && e["payload"]["kind"] == "assistant_text"
        }),
        "{events:?}"
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn thinking_channel_markup_is_hidden_from_transcript_events() {
    let server = support::server(|_, _| {
        (
            response(
                "thought <channel|>private plan\n</channel>\nFixed the typo in main.rs.",
                json!([]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let job = engine
        .start(request(root.path(), "Fix the typo", None))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "completed", "{}", result.summary);
    assert!(result.summary.contains("Fixed the typo"));
    assert!(!result.summary.to_ascii_lowercase().contains("thought"));
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    let deltas: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "model.delta")
        .filter_map(|e| e["payload"]["text"].as_str())
        .collect();
    assert!(!deltas.is_empty());
    assert!(deltas
        .iter()
        .all(|text| !text.to_ascii_lowercase().contains("thought")));
    assert!(deltas.iter().all(|text| !text.contains("<channel")));
    assert!(deltas.iter().any(|text| text.contains("Fixed the typo")));
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn prose_command_without_tool_is_nudged_then_pauses() {
    let server = support::server(|_, _| {
        (
            response(
                "I'll run xset root solid green on all three monitors now.",
                json!([]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    Config::patch(
        engine.paths(),
        json!({"agent":{"max_steps":8,"max_fix_retries":1}}),
    )
    .unwrap();
    let job = engine
        .start(request(root.path(), "Make the screens green", None))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "failed", "{}", result.summary);
    assert!(
        result.summary.contains("never called a tool")
            || result.summary.contains("paused")
            || result
                .summary
                .contains("promised further work without performing it"),
        "{}",
        result.summary
    );
    assert_eq!(
        server.requests.lock().unwrap().len(),
        2,
        "a command promise respects max_fix_retries=1"
    );
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    assert!(
        events
            .iter()
            .any(|e| { e["type"] == "runaway.warning" && e["payload"]["kind"] == "prose_command" }),
        "{events:?}"
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn short_normal_answer_still_completes() {
    let server = support::server(|_, _| {
        (
            response("Updated README with the install steps.", json!([])),
            Duration::ZERO,
        )
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let job = engine
        .start(request(root.path(), "Summarize the change", None))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "completed", "{}", result.summary);
    assert!(result.summary.contains("Updated README"));
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn completion_promise_after_failed_check_follows_through_without_automatic_shell_replay() {
    let server = support::server(|index, body| {
        let messages = body["messages"].as_array().unwrap();
        context::validate_pairs(messages).unwrap();
        let answer = match index {
            0 => response("Inspecting and checking the current implementation", json!([
                tool("read", "read_file", json!({"path":"sum.sh"})),
                tool("check-before", "exec", json!({"command":"sh check.sh"})),
            ])),
            1 => {
                let result: Value = serde_json::from_str(messages.last().unwrap()["content"].as_str().unwrap()).unwrap();
                assert_eq!(result["success"], false, "the model must receive the actual failed check");
                response("I will make the necessary changes to the implementation, then run tests again.", json!([]))
            }
            2 => {
                assert_eq!(messages.last().unwrap()["role"], "system", "an explicit completion reminder must ask the model to continue");
                response("Applying the correction and checking it", json!([
                    tool("fix", "edit_file", json!({"path":"sum.sh","old_string":"a - b","new_string":"a + b"})),
                    tool("check-after", "exec", json!({"command":"sh check.sh"})),
                ]))
            }
            _ => {
                let result: Value = serde_json::from_str(messages.last().unwrap()["content"].as_str().unwrap()).unwrap();
                assert_eq!(result["success"], true);
                response("Fixed addition. The configured shell assertion passed.", json!([]))
            }
        };
        (answer, Duration::ZERO)
    }).await;
    let (root, engine) = setup(&server.endpoint);
    let project = root.path().join("project");
    // Record execution count as generated output, not as a checked source
    // input: a check that changes its own input must correctly go stale.
    fs::create_dir(project.join("target")).unwrap();
    fs::write(project.join("sum.sh"), "a=$1; b=$2; echo $((a - b))\n").unwrap();
    fs::write(
        project.join("check.sh"),
        "printf 'run\\n' >> target/check-runs\ntest \"$(sh sum.sh 2 3)\" = 5\n",
    )
    .unwrap();
    let hook_counter = root.path().join("completion-hook-count");
    approve_completion_counter(&engine, &project, &hook_counter);
    Config::patch(
        engine.paths(),
        json!({"verification":{"commands":["sh check.sh"]},"agent":{"max_fix_retries":2}}),
    )
    .unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Fix sum.sh so it adds its two arguments, then run sh check.sh",
            None,
        ))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(result.status, "completed", "{}", result.summary);
    assert!(
        fs::read_to_string(project.join("sum.sh"))
            .unwrap()
            .contains("a + b"),
        "the task ended before performing the promised repair: {}",
        result.summary
    );
    assert_eq!(
        fs::read_to_string(project.join("target/check-runs"))
            .unwrap()
            .lines()
            .count(),
        2,
        "each check must run only when the model explicitly calls it"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 4);
    assert_eq!(
        fs::read_to_string(hook_counter).unwrap(),
        "x",
        "completion hook runs only after the real final answer"
    );
    let retries: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "completion.retry")
        .collect();
    assert_eq!(retries.len(), 1, "{events:?}");
    assert_eq!(retries[0]["payload"]["reason"], "unperformed_action");
    let checks: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "tool.completed" && e["payload"]["tool"] == "exec")
        .collect();
    assert_eq!(
        checks.len(),
        2,
        "no completion path silently repeats the failed command"
    );
    let verification = &result.result.as_ref().unwrap()["verification"];
    assert_eq!(verification["status"], "passed", "{verification}");
    assert_eq!(verification["red_green"], true, "{verification}");
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "agent.completed")
            .count(),
        1
    );
    context::validate_pairs(&engine.store().messages(&job.id).unwrap()).unwrap();
}

#[tokio::test]
async fn completion_promise_to_edit_continues_until_the_file_is_written() {
    let server = support::server(|index, body| {
        let messages = body["messages"].as_array().unwrap();
        context::validate_pairs(messages).unwrap();
        let answer = match index {
            0 => response(
                "I'll make the necessary changes to the requested file.",
                json!([]),
            ),
            1 => response(
                "Creating the requested greeting",
                json!([tool(
                    "write",
                    "write_file",
                    json!({"path":"greeting.txt","content":"hello\n","expected_hash":"missing"})
                ),]),
            ),
            _ => response("Created greeting.txt containing hello.", json!([])),
        };
        (answer, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    Config::patch(engine.paths(), json!({"agent":{"max_fix_retries":2}})).unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Create greeting.txt containing hello",
            None,
        ))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    let events = engine.store().recent_events(&job.session_id, 100).unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(result.status, "completed", "{}", result.summary);
    assert!(
        root.path().join("project/greeting.txt").is_file(),
        "a prose edit promise was accepted as completion: {}",
        result.summary
    );
    assert_eq!(
        fs::read_to_string(root.path().join("project/greeting.txt")).unwrap(),
        "hello\n"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 3);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "completion.retry"
                && e["payload"]["reason"] == "unperformed_action")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "tool.completed"
                && e["payload"]["tool"] == "write_file"
                && e["payload"]["success"] == true)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "agent.completed")
            .count(),
        1
    );
}

#[tokio::test]
async fn completion_persistent_edit_promise_fails_at_max_fix_retries_once() {
    let server = support::server(|_, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        (
            response(
                "I will make the necessary changes to the requested file.",
                json!([]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    Config::patch(
        engine.paths(),
        json!({"agent":{"max_steps":10,"max_fix_retries":2}}),
    )
    .unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Create greeting.txt containing hello",
            None,
        ))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    let events = engine.store().recent_events(&job.session_id, 100).unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(
        result.status, "failed",
        "an unperformed promise must not finish successfully: {}",
        result.summary
    );
    assert_eq!(
        server.requests.lock().unwrap().len(),
        3,
        "initial response plus two bounded repair requests"
    );
    assert_eq!(result.steps, 3);
    let retries: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "completion.retry")
        .collect();
    assert_eq!(retries.len(), 2, "{events:?}");
    for (index, event) in retries.iter().enumerate() {
        assert_eq!(event["payload"]["reason"], "unperformed_action");
        assert_eq!(event["payload"]["attempt"], index + 1);
    }
    let terminal: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "agent.completed")
        .collect();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0]["payload"]["success"], false);
    assert!(!root.path().join("project/greeting.txt").exists());
    assert!(!events.iter().any(|e| e["type"] == "tool.started"));
}

#[tokio::test]
async fn completion_failed_check_receipt_survives_bounded_promise_failure() {
    for max_fix_retries in [0usize, 2] {
        let server = support::server(|index, body| {
            context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
            let answer = if index == 0 {
                response("Checking the current implementation", json!([
                    tool("failed-check", "exec", json!({"command":"sh check.sh"})),
                ]))
            } else {
                response("I will make the necessary changes to the implementation, then run tests again.", json!([]))
            };
            (answer, Duration::ZERO)
        }).await;
        let (root, engine) = setup(&server.endpoint);
        let project = root.path().join("project");
        fs::write(
            project.join("check.sh"),
            "printf 'run\\n' >> check-runs\nexit 1\n",
        )
        .unwrap();
        let hook_counter = root.path().join("completion-hook-count");
        approve_completion_counter(&engine, &project, &hook_counter);
        Config::patch(engine.paths(), json!({"verification":{"commands":["sh check.sh"]},"agent":{"max_steps":10,"max_fix_retries":max_fix_retries}})).unwrap();
        let job = engine
            .start(request(
                root.path(),
                "Fix the failing implementation and rerun sh check.sh",
                None,
            ))
            .await
            .unwrap();
        let result = wait(&engine, &job.id).await;
        let events = engine.store().recent_events(&job.session_id, 200).unwrap();
        engine.shutdown().await.unwrap();
        assert_eq!(
            result.status, "failed",
            "retry bound {max_fix_retries}: {}",
            result.summary
        );
        assert!(
            !hook_counter.exists(),
            "completion hooks do not run for promise retries or refusal"
        );
        assert_eq!(
            server.requests.lock().unwrap().len(),
            max_fix_retries + 2,
            "one tool response, one promise and the allowed repair attempts"
        );
        assert_eq!(
            fs::read_to_string(project.join("check-runs"))
                .unwrap()
                .lines()
                .count(),
            1,
            "completion must not replay the check"
        );
        let verification = &result.result.as_ref().unwrap()["verification"];
        assert_eq!(verification["status"], "failed");
        assert_eq!(verification["verified"], false);
        let commands = verification["commands"]
            .as_array()
            .expect("recorded failure must survive the completion error");
        assert_eq!(commands.len(), 1, "{verification}");
        assert_eq!(commands[0]["command"], "sh check.sh");
        assert_eq!(commands[0]["state"], "failed");
        assert_eq!(commands[0]["tool_call_id"], "failed-check");
        assert_eq!(commands[0]["task_id"], job.task_id);
        assert_eq!(commands[0]["attempt_id"], job.id);
        assert_eq!(commands[0]["provenance"], "locally_observed");
        assert_eq!(
            events
                .iter()
                .filter(|e| e["type"] == "completion.retry"
                    && e["payload"]["reason"] == "unperformed_action")
                .count(),
            max_fix_retries
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e["type"] == "tool.started" && e["payload"]["tool"] == "exec")
                .count(),
            1
        );
        let terminals: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "agent.completed")
            .collect();
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0]["payload"]["success"], false);
        let saved = engine
            .store()
            .last_task_event(&job.task_id, "verification.summary")
            .unwrap()
            .expect("the failed completion retains its typed evidence event");
        assert_eq!(saved["payload"]["commands"][0], commands[0]);
    }
}

#[tokio::test]
async fn completion_retry_budget_is_not_reset_by_interleaved_read_only_tools() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let answer = if index % 2 == 1 {
            response(
                "Reading the current value",
                json!([tool(
                    &format!("read-{index}"),
                    "read_file",
                    json!({"path":"source.txt"})
                ),]),
            )
        } else {
            response(
                "I will make the necessary changes to the requested file.",
                json!([]),
            )
        };
        (answer, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    fs::write(root.path().join("project/source.txt"), "old\n").unwrap();
    Config::patch(
        engine.paths(),
        json!({"agent":{"max_steps":12,"max_fix_retries":2}}),
    )
    .unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Update source.txt to contain new",
            None,
        ))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(result.status, "failed", "{}", result.summary);
    assert_eq!(
        server.requests.lock().unwrap().len(),
        5,
        "three promises and two read-only tool responses exhaust two repair retries"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("project/source.txt")).unwrap(),
        "old\n"
    );
    let retries: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "completion.retry")
        .collect();
    assert_eq!(retries.len(), 2);
    assert_eq!(retries[0]["payload"]["attempt"], 1);
    assert_eq!(retries[1]["payload"]["attempt"], 2);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "tool.completed" && e["payload"]["tool"] == "read_file")
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "agent.completed")
            .count(),
        1
    );
}

#[tokio::test]
async fn completion_mixed_edit_and_command_promises_share_one_retry_budget_and_keep_receipts() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let answer = match index {
            0 => response(
                "Checking the current implementation",
                json!([tool(
                    "failed-check",
                    "exec",
                    json!({"command":"sh check.sh"})
                ),]),
            ),
            1 => response(
                "I'll make the necessary changes to the implementation.",
                json!([]),
            ),
            2 | 4 => response(
                "Reading current source",
                json!([tool(
                    &format!("read-{index}"),
                    "read_file",
                    json!({"path":"source.txt"})
                ),]),
            ),
            _ => response("I'll run tests now.", json!([])),
        };
        (answer, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let project = root.path().join("project");
    fs::write(project.join("source.txt"), "unchanged\n").unwrap();
    fs::write(
        project.join("check.sh"),
        "printf 'run\\n' >> check-runs\nexit 1\n",
    )
    .unwrap();
    Config::patch(engine.paths(), json!({"verification":{"commands":["sh check.sh"]},"agent":{"max_steps":12,"max_fix_retries":2}})).unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Fix the implementation in source.txt and rerun sh check.sh",
            None,
        ))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(result.status, "failed", "{}", result.summary);
    assert_eq!(
        server.requests.lock().unwrap().len(),
        6,
        "switching to command promises or reading files must not reset or bypass the repair budget"
    );
    assert_eq!(result.steps, 6);
    assert_eq!(
        fs::read_to_string(project.join("check-runs"))
            .unwrap()
            .lines()
            .count(),
        1,
        "a prose command promise cannot cause shell replay"
    );
    assert_eq!(
        fs::read_to_string(project.join("source.txt")).unwrap(),
        "unchanged\n"
    );
    let verification = &result.result.as_ref().unwrap()["verification"];
    let commands = verification["commands"]
        .as_array()
        .expect("the unified refusal retains the failed receipt");
    assert_eq!(commands.len(), 1, "{verification}");
    assert_eq!(commands[0]["state"], "failed");
    assert_eq!(commands[0]["tool_call_id"], "failed-check");
    assert_eq!(commands[0]["task_id"], job.task_id);
    assert_eq!(commands[0]["attempt_id"], job.id);
    assert_eq!(commands[0]["provenance"], "locally_observed");
    let retries: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "completion.retry")
        .collect();
    assert_eq!(retries.len(), 2);
    for (index, event) in retries.iter().enumerate() {
        assert_eq!(event["payload"]["reason"], "unperformed_action");
        assert_eq!(event["payload"]["attempt"], index + 1);
    }
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "runaway.warning" && e["payload"]["kind"] == "prose_command"),
        "command promises retain the existing visible warning"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "tool.completed" && e["payload"]["tool"] == "read_file")
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "agent.completed")
            .count(),
        1
    );
    let saved = engine
        .store()
        .last_task_event(&job.task_id, "verification.summary")
        .unwrap()
        .unwrap();
    assert_eq!(saved["payload"]["commands"][0], commands[0]);
}

#[tokio::test]
async fn completion_quoted_conditional_and_negative_commands_are_final_prose_in_code_and_review() {
    let answers = [
        "The example says \"I'll run tests now.\" It describes a future action, not observed execution.",
        "```text\nI'll run tests now.\n```\nThis is quoted sample wording, not work performed.",
        "If you approve, I'll run tests. No commands have been executed.",
        "I'll run nothing. This answer only explains the wording.",
    ];
    for mode in ["code", "review"] {
        for answer in answers {
            let server = support::server(move |_, body| {
                context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
                (response(answer, json!([])), Duration::ZERO)
            })
            .await;
            let (root, engine) = setup(&server.endpoint);
            Config::patch(engine.paths(), json!({"agent":{"max_fix_retries":1}})).unwrap();
            let mut req = request(
                root.path(),
                "Explain this wording only; do not execute commands or modify files.",
                None,
            );
            req.mode = mode.into();
            let job = engine.start(req).await.unwrap();
            let result = wait(&engine, &job.id).await;
            let events = engine.store().recent_events(&job.session_id, 100).unwrap();
            engine.shutdown().await.unwrap();
            assert_eq!(
                result.status, "completed",
                "{mode}: {answer:?}: {}",
                result.summary
            );
            assert_eq!(
                server.requests.lock().unwrap().len(),
                1,
                "{mode}: {answer:?}"
            );
            assert!(
                !events.iter().any(|e| e["type"] == "tool.started"
                    || e["type"] == "completion.retry"
                    || (e["type"] == "runaway.warning" && e["payload"]["kind"] == "prose_command")),
                "{mode}: {answer:?}: {events:?}"
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e["type"] == "agent.completed")
                    .count(),
                1
            );
        }
    }
}

#[tokio::test]
async fn completion_diagnosis_can_report_a_failed_check_without_repairing_or_replaying_it() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let answer = if index == 0 {
            response("Running the requested diagnostic check", json!([
                tool("diagnose", "exec", json!({"command":"sh check.sh"})),
            ]))
        } else {
            response("The check failed because addition returned -1 instead of 5. The implementation needs addition rather than subtraction. I have not modified it.", json!([]))
        };
        (answer, Duration::ZERO)
    }).await;
    let (root, engine) = setup(&server.endpoint);
    let project = root.path().join("project");
    fs::write(project.join("sum.sh"), "a=$1; b=$2; echo $((a - b))\n").unwrap();
    fs::write(
        project.join("check.sh"),
        "printf 'run\\n' >> check-runs\ntest \"$(sh sum.sh 2 3)\" = 5\n",
    )
    .unwrap();
    Config::patch(
        engine.paths(),
        json!({"verification":{"commands":["sh check.sh"]},"agent":{"max_fix_retries":2}}),
    )
    .unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Run sh check.sh and diagnose its failure. Do not repair or edit the implementation.",
            None,
        ))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    let events = engine.store().recent_events(&job.session_id, 100).unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(result.status, "completed", "{}", result.summary);
    assert_eq!(
        result.result.as_ref().unwrap()["verification"]["status"],
        "failed"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert_eq!(
        fs::read_to_string(project.join("check-runs"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(fs::read_to_string(project.join("sum.sh"))
        .unwrap()
        .contains("a - b"));
    assert!(!events.iter().any(|e| e["type"] == "completion.retry"));
}

#[tokio::test]
async fn completion_read_only_plan_accepts_prose_without_forcing_a_write() {
    let server = support::server(|_, _| (
        response("I will update input validation, then add unit tests for empty values. This is a proposed implementation plan; no files were changed.", json!([])), Duration::ZERO
    )).await;
    let (root, engine) = setup(&server.endpoint);
    let mut req = request(
        root.path(),
        "Suggest a plan for improving input validation. Do not modify files.",
        None,
    );
    req.mode = "review".into();
    let job = engine.start(req).await.unwrap();
    let result = wait(&engine, &job.id).await;
    let events = engine.store().recent_events(&job.session_id, 100).unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(result.status, "completed", "{}", result.summary);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert!(!events
        .iter()
        .any(|e| e["type"] == "completion.retry" || e["type"] == "tool.started"));
}

#[tokio::test]
async fn model_success_claim_without_tests_is_not_verified() {
    let server = support::server(|_, _| {
        (
            response(
                "All tests passed. The feature is correctly implemented.",
                json!([]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let job = engine
        .start(request(root.path(), "Say the tests passed", None))
        .await
        .unwrap();
    let result = wait(&engine, &job.id).await;
    assert_eq!(result.status, "completed", "{}", result.summary);
    let summary = engine
        .store()
        .last_task_event(&job.task_id, "verification.summary")
        .unwrap()
        .unwrap();
    assert_eq!(summary["payload"]["verified"], false);
    assert_eq!(summary["payload"]["unverified_claim"], true);
    assert_eq!(summary["payload"]["claim"], "model_claim");
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn event_fork_keeps_completed_context_without_future_turns() {
    let server = support::server(|index, _| {
        (
            response(
                if index == 0 {
                    "First answer: amber."
                } else {
                    "Future answer: violet."
                },
                json!([]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let first = engine
        .start(request(root.path(), "Remember amber", None))
        .await
        .unwrap();
    assert_eq!(wait(&engine, &first.id).await.status, "completed");
    let cut = engine.store().event_cursor(&first.session_id).unwrap();
    let second = engine
        .start(request(
            root.path(),
            "Now remember violet",
            Some(first.session_id.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(wait(&engine, &second.id).await.status, "completed");
    let fork = engine
        .store()
        .fork_session_from_event(&first.session_id, cut, "Earlier state")
        .unwrap();
    let sid = fork["fork"]["id"].as_str().unwrap();
    let tape = engine.store().latest_session_messages(sid, "").unwrap();
    context::validate_pairs(&tape).unwrap();
    assert!(json!(tape).to_string().contains("amber"));
    assert!(!json!(tape).to_string().contains("violet"));
    assert!(engine
        .store()
        .fork_session_from_event(sid, cut, "Wrong event ownership")
        .is_err());
    engine.store().delete_session(&first.session_id).unwrap();
    assert!(
        json!(engine.store().latest_session_messages(sid, "").unwrap())
            .to_string()
            .contains("amber")
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn command_names_do_not_verify_and_configured_receipts_go_stale_after_edits() {
    for (configured, edit_after, expected) in [
        (false, false, "not_run"),
        (true, false, "passed"),
        (true, true, "stale"),
    ] {
        let server = support::server(move |index, _| {
            let answer = if index == 0 {
                response(
                    "Running command",
                    json!([tool("check", "exec", json!({"command":"printf test"}))]),
                )
            } else if index == 1 && edit_after {
                response(
                    "Editing",
                    json!([tool(
                        "edit",
                        "write_file",
                        json!({"path":"source.txt","content":"changed","expected_hash":"missing"})
                    )]),
                )
            } else {
                response("All tests passed.", json!([]))
            };
            (answer, Duration::ZERO)
        })
        .await;
        let (root, engine) = setup(&server.endpoint);
        if configured {
            Config::patch(
                engine.paths(),
                json!({"verification":{"commands":["printf test"]}}),
            )
            .unwrap();
        }
        let job = engine
            .start(request(root.path(), "Run the configured operation", None))
            .await
            .unwrap();
        let result = wait(&engine, &job.id).await;
        assert_eq!(result.status, "completed", "{}", result.summary);
        let evidence = &result.result.as_ref().unwrap()["verification"];
        assert_eq!(evidence["status"], expected, "{evidence}");
        assert_eq!(evidence["verified"], configured && !edit_after);
        let receipt = &evidence["commands"][0];
        assert_eq!(receipt["task_id"], job.task_id);
        assert_eq!(receipt["attempt_id"], job.id);
        assert_eq!(receipt["tool_call_id"], "check");
        assert_eq!(receipt["exit_code"], 0);
        assert_eq!(receipt["provenance"], "locally_observed");
        assert!(receipt["output_ref"]
            .as_str()
            .unwrap()
            .starts_with("event:"));
        if configured {
            assert!(receipt["workspace_fingerprint"].is_string());
        }
        // Even a configured custom check makes no assertion that a test suite ran.
        assert!(receipt["scope"].as_str().unwrap().contains(if configured {
            "No assertion about test counts"
        } else {
            "not verification"
        }));
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn timings_separate_model_attempts_from_check_processes_and_survive_restart() {
    let server = support::server(|index, _| {
        let result = if index == 0 {
            response(
                "",
                json!([tool("check", "exec", json!({"command":"sleep 0.15"}))]),
            )
        } else {
            response("Finished the requested check.", json!([]))
        };
        (result, Duration::from_millis(120))
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    Config::patch(
        engine.paths(),
        json!({"verification":{"commands":["sleep 0.15"]}}),
    )
    .unwrap();
    let job = engine
        .start(request(root.path(), "Run the configured check", None))
        .await
        .unwrap();
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    let t = done.timings.as_ref().unwrap();
    assert!(t.complete);
    assert_eq!(t.model_requests, 2);
    assert_eq!(t.first_text_request, Some(2)); // Tool-only response is not text.
    assert!(t.first_text_seconds.unwrap() >= 0.1); // Buffered JSON is documented as such.
    assert!(t.model_requests_seconds.unwrap() >= 0.2);
    assert!(t.check_process_seconds.unwrap() >= 0.1);
    assert!(t.tool_batches_seconds.unwrap() >= t.check_process_seconds.unwrap());
    assert!(
        t.total_seconds
            >= t.queue_seconds
                + t.model_requests_seconds.unwrap()
                + t.tool_batches_seconds.unwrap()
    );
    assert!(t.model_load_seconds.is_none()); // External endpoint, no invented load.
    let events = engine
        .store()
        .events_after(&job.session_id, 0, None, 10000)
        .unwrap();
    let receipts: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "model.request_timing")
        .collect();
    assert_eq!(receipts.len(), 2);
    assert!(receipts[0]["payload"]["first_text_seconds"].is_null());
    assert!(
        receipts[1]["payload"]["first_text_seconds"]
            .as_f64()
            .unwrap()
            >= 0.1
    );
    let sum: f64 = receipts
        .iter()
        .map(|e| e["payload"]["elapsed_seconds"].as_f64().unwrap())
        .sum();
    assert!((t.model_requests_seconds.unwrap() - sum).abs() < 0.02);
    let completed = events
        .iter()
        .find(|e| e["type"] == "agent.completed")
        .unwrap();
    let expected = serde_json::to_value(t).unwrap();
    assert_eq!(completed["payload"]["timings"], expected);
    let paths = engine.paths().clone();
    engine.shutdown().await.unwrap();
    drop(engine);
    let reopened = Engine::open(paths).unwrap();
    assert_eq!(
        serde_json::to_value(reopened.job(&job.id).unwrap().unwrap().timings).unwrap(),
        expected
    );
    reopened.shutdown().await.unwrap();
}

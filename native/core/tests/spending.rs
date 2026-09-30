//! Spending limits with a fake paid model on loopback (provider
//! `openrouter`, which is always billed per token, answering with
//! `usage.cost`): a task waits between model turns at its limit, continues
//! with a raised limit or stops cleanly, the daily total spans tasks,
//! subagents count toward the task that started them, local models are never
//! limited, and the same flow works headless through the service routes.
#![cfg(unix)]
mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    engine::{Engine, Job, StartRequest},
    paths::AppPaths,
    service::{Request, Service},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn tool(name: &str, args: Value) -> Value {
    json!({"id":shadowcode_core::id(),"type":"function","function":{"name":name,"arguments":args.to_string()}})
}

/// A completion that cost `cost` US dollars, as OpenRouter reports it.
fn reply(text: &str, calls: Value, cost: f64) -> Value {
    let reason = if calls.as_array().is_some_and(|c| !c.is_empty()) {
        "tool_calls"
    } else {
        "stop"
    };
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":reason}],
        "usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"cost":cost}})
}

fn setup(endpoint: &str, provider: &str, spending: Value) -> (tempfile::TempDir, Engine, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let project = project.canonicalize().unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({
            "model":{"provider":provider,"endpoint":endpoint,"name":"acme/coder","api_key_env":"SHADOWCODE_TEST_UNUSED_API_KEY","context_limit":32768},
            "trusted_workspaces":[project],
            "permissions":{"approve_shell":false,"mode":"allow_edits"},
            "cli_agents":{"enabled":false},
            "agent":{"max_steps":12,"model_retries":0},
            "spending":spending,
        }),
    )
    .unwrap();
    (root, Engine::open(paths).unwrap(), project)
}

fn request(project: &Path, task: &str, session_id: Option<String>) -> StartRequest {
    StartRequest {
        workspace: project.to_path_buf(),
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
    tokio::time::timeout(Duration::from_secs(120), engine.wait(id))
        .await
        .expect("job did not finish in time")
        .unwrap()
}

fn events(engine: &Engine, session: &str, kind: &str) -> Vec<Value> {
    engine
        .store()
        .recent_events(session, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e["type"] == kind)
        .collect()
}

/// The limit card of a running task, once it shows.
async fn card(engine: &Engine, session: &str) -> Value {
    let started = Instant::now();
    loop {
        if let Some(event) = events(engine, session, "spend.limit_reached").pop() {
            return event;
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "no limit card appeared: {:?}",
            engine
                .store()
                .recent_events(session, 1000)
                .unwrap()
                .iter()
                .map(|e| (e["type"].clone(), e["payload"]["text"].clone()))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// Three steps of about $0.08, $0.04 and $0.01 each.
async fn three_step_server() -> support::Server {
    support::server(|index, _| {
        let response = match index {
            0 => reply(
                "Looking.",
                json!([tool("list_files", json!({"path":"."}))]),
                0.08,
            ),
            1 => reply(
                "Looking again.",
                json!([tool("list_files", json!({"path":"."}))]),
                0.04,
            ),
            _ => reply("All done.", json!([]), 0.01),
        };
        (response, Duration::ZERO)
    })
    .await
}

#[tokio::test]
async fn a_paid_task_waits_between_turns_then_continues_with_a_raised_limit() {
    let server = three_step_server().await;
    let (_root, engine, project) = setup(
        &server.endpoint,
        "openrouter",
        json!({"task_usd":0.1,"daily_usd":null}),
    );
    let job = engine
        .start(request(&project, "List the files", None))
        .await
        .unwrap();
    let card = card(&engine, &job.session_id).await;
    assert_eq!(card["task_id"], job.task_id.as_str());
    let payload = &card["payload"];
    assert_eq!(payload["kind"], "task");
    assert_eq!(payload["limit"], 0.1);
    assert_eq!(payload["raise_to"], 0.2);
    assert_eq!(
        payload["continue_label"],
        "Continue (limit raised to $0.20)"
    );
    assert!(payload["text"]
        .as_str()
        .unwrap()
        .contains("It has spent $0.12 on paid models"));
    // Waiting between turns: no third request, both tool calls finished,
    // and the task is still running.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let tools = events(&engine, &job.session_id, "tool.completed").len();
    assert_eq!(tools, 2, "no tool call is interrupted");
    assert_eq!(engine.job(&job.id).unwrap().unwrap().status, "running");
    // The 75% notice came first, once.
    let notices = events(&engine, &job.session_id, "spend.notice");
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0]["payload"]["text"],
        "This task has spent $0.08 of its $0.10 limit on paid models."
    );
    let waiting = engine.spending_waiting().unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0]["session_id"], job.session_id.as_str());
    let answer = engine
        .decide_spending(&job.id, payload["id"].as_str().unwrap(), "continue")
        .unwrap();
    assert_eq!(answer["limit"], 0.2);
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    assert_eq!(server.requests.lock().unwrap().len(), 3);
    assert!((done.usage.cost_usd.unwrap() - 0.13).abs() < 1e-9);
    assert_eq!(
        events(&engine, &job.session_id, "spend.limit_resolved").len(),
        1
    );
    assert!(engine.spending_waiting().unwrap().is_empty());
    // The run record says exactly what ran.
    let run = done.run.expect("run record");
    assert_eq!(run.provider, "openrouter");
    assert_eq!(run.model, "acme/coder");
    assert_eq!(run.app_version, shadowcode_core::VERSION);
    assert_eq!(run.settings_hash.len(), 12);
    let finished = events(&engine, &job.session_id, "agent.completed")
        .pop()
        .unwrap();
    assert_eq!(finished["payload"]["run"]["provider"], "openrouter");
    // The day's total counts every paid request.
    let day = shadowcode_core::spending::today(&engine.store(), shadowcode_core::now()).unwrap();
    assert!((day.usd - 0.13).abs() < 1e-9);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn stop_at_the_limit_ends_the_task_cleanly_without_another_request() {
    let server = three_step_server().await;
    let (_root, engine, project) = setup(
        &server.endpoint,
        "openrouter",
        json!({"task_usd":0.1,"daily_usd":null}),
    );
    let job = engine
        .start(request(&project, "List the files", None))
        .await
        .unwrap();
    let card = card(&engine, &job.session_id).await;
    assert!(engine
        .decide_spending(&job.id, "not-this-card", "stop")
        .is_err());
    engine
        .decide_spending(&job.id, card["payload"]["id"].as_str().unwrap(), "stop")
        .unwrap();
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "cancelled");
    assert_eq!(
        done.summary,
        "Stopped at your per-task spending limit. Changes made so far remain available for review or rewind."
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let resolved = events(&engine, &job.session_id, "spend.limit_resolved");
    assert_eq!(resolved[0]["payload"]["action"], "stop");
    assert!(engine.decide_spending(&job.id, "x", "continue").is_err());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn models_on_this_computer_are_never_limited() {
    let server = three_step_server().await;
    // A loopback endpoint that is not OpenRouter runs on this computer.
    let (_root, engine, project) = setup(
        &server.endpoint,
        "local",
        json!({"task_usd":0.01,"daily_usd":0.01}),
    );
    let job = engine
        .start(request(&project, "List the files", None))
        .await
        .unwrap();
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    assert_eq!(server.requests.lock().unwrap().len(), 3);
    for kind in ["spend.notice", "spend.limit_reached", "spend.unknown"] {
        assert!(events(&engine, &job.session_id, kind).is_empty(), "{kind}");
    }
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_daily_limit_spans_tasks_and_continue_raises_it_for_today() {
    let server = support::server(|_, _| (reply("Done.", json!([]), 0.06), Duration::ZERO)).await;
    let (_root, engine, project) = setup(
        &server.endpoint,
        "openrouter",
        json!({"task_usd":null,"daily_usd":0.05}),
    );
    let first = engine
        .start(request(&project, "First", None))
        .await
        .unwrap();
    assert_eq!(wait(&engine, &first.id).await.status, "completed");
    // The next task, in another conversation, starts over the day's limit.
    let second = engine
        .start(request(&project, "Second", None))
        .await
        .unwrap();
    let card = card(&engine, &second.session_id).await;
    assert_eq!(card["payload"]["kind"], "daily");
    assert_eq!(card["payload"]["raise_to"], 0.1);
    assert!(card["payload"]["resets_at"].as_f64().unwrap() > shadowcode_core::now());
    assert_eq!(
        server.requests.lock().unwrap().len(),
        1,
        "waits before asking"
    );
    engine
        .decide_spending(
            &second.id,
            card["payload"]["id"].as_str().unwrap(),
            "continue",
        )
        .unwrap();
    assert_eq!(wait(&engine, &second.id).await.status, "completed");
    let day = shadowcode_core::spending::today(&engine.store(), shadowcode_core::now()).unwrap();
    assert_eq!(day.raised_to, Some(0.1));
    assert!((day.usd - 0.12).abs() < 1e-9);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn subagents_count_toward_the_task_that_started_them() {
    let seen = Arc::new(Mutex::new(0usize));
    let counter = seen.clone();
    let server = support::server(move |_, body| {
        *counter.lock().unwrap() += 1;
        let system = body["messages"][0]["content"].as_str().unwrap_or("");
        let child = system.contains("You are the subagent '");
        let tool_turns = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .count();
        let response = match (child, tool_turns) {
            // The parent starts one subagent.
            (false, 0) => reply(
                "Delegating.",
                json!([tool(
                    "spawn_agent",
                    json!({"agent":"explore","prompt":"Look around"})
                )]),
                0.02,
            ),
            // The child spends past the parent task's limit in one step.
            (true, 0) => reply(
                "Listing.",
                json!([tool("list_files", json!({"path":"."}))]),
                0.04,
            ),
            (true, _) => reply("Found nothing.", json!([]), 0.01),
            (false, _) => reply("Finished.", json!([]), 0.01),
        };
        (response, Duration::ZERO)
    })
    .await;
    let (_root, engine, project) = setup(
        &server.endpoint,
        "openrouter",
        json!({"task_usd":0.05,"daily_usd":null}),
    );
    let job = engine
        .start(request(&project, "Explore", None))
        .await
        .unwrap();
    // The card shows in the parent task's conversation, not the child's.
    let card = card(&engine, &job.session_id).await;
    assert_eq!(card["task_id"], job.task_id.as_str());
    assert_eq!(card["payload"]["job_id"], job.id.as_str());
    assert!((card["payload"]["spent"].as_f64().unwrap() - 0.06).abs() < 1e-9);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        *seen.lock().unwrap(),
        2,
        "the child waits between its turns"
    );
    engine
        .decide_spending(&job.id, card["payload"]["id"].as_str().unwrap(), "stop")
        .unwrap();
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "cancelled", "{}", done.summary);
    assert!(done
        .summary
        .starts_with("Stopped at your per-task spending limit"));
    assert_eq!(*seen.lock().unwrap(), 2);
    engine.shutdown().await.unwrap();
}

async fn call(service: &Service, method: &str, path: &str, body: Value) -> anyhow::Result<Value> {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
}

#[tokio::test]
async fn headless_clients_see_the_card_answer_it_and_get_an_estimate() {
    let server = three_step_server().await;
    let (root, engine, project) = setup(
        &server.endpoint,
        "openrouter",
        json!({"task_usd":5.0,"daily_usd":null}),
    );
    let paths = engine.paths().clone();
    engine.shutdown().await.unwrap();
    drop(engine);
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    // `--max-cost` replaces the setting for this task only.
    let bad = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"workspace":project,"task":"List","max_cost_usd":-1}),
    )
    .await;
    assert!(bad.is_err());
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"workspace":project,"task":"List","max_cost_usd":0.1}),
    )
    .await
    .unwrap();
    let (id, session) = (
        job["id"].as_str().unwrap().to_owned(),
        job["session_id"].as_str().unwrap().to_owned(),
    );
    let started = Instant::now();
    let waiting = loop {
        let status = call(&service, "GET", "/api/spending", Value::Null)
            .await
            .unwrap();
        if let Some(card) = status["waiting"].as_array().and_then(|w| w.first()) {
            assert_eq!(status["limits"]["task_usd"], 5.0);
            assert!((status["today"]["usd"].as_f64().unwrap() - 0.12).abs() < 1e-9);
            break card.clone();
        }
        assert!(started.elapsed() < Duration::from_secs(120), "{status}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert_eq!(waiting["session_id"], session.as_str());
    assert_eq!(waiting["limit"], 0.1);
    // The sidebar marks the conversation as waiting, and the card's events
    // wake the window's feed.
    let feed = call(&service, "GET", "/api/feed", Value::Null)
        .await
        .unwrap();
    assert_eq!(feed["waiting"], json!([session]), "{feed}");
    for kind in ["spend.limit_reached", "spend.limit_resolved"] {
        assert!(
            feed["events"].as_array().unwrap().contains(&json!(kind)),
            "{kind}"
        );
    }
    let answer = call(
        &service,
        "POST",
        &format!("/api/jobs/{id}/spending"),
        json!({"prompt_id":waiting["id"],"action":"stop"}),
    )
    .await
    .unwrap();
    assert_eq!(answer["action"], "stop");
    let started = Instant::now();
    let done = loop {
        let job = call(&service, "GET", &format!("/api/jobs/{id}"), Value::Null)
            .await
            .unwrap();
        if job["status"] == "cancelled" {
            break job;
        }
        assert!(started.elapsed() < Duration::from_secs(120), "{job}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert!(done["summary"]
        .as_str()
        .unwrap()
        .starts_with("Stopped at your per-task spending limit"));

    // The estimate: shown for a paid model with known prices, hidden
    // otherwise.
    fs::write(
        paths.state.join("openrouter-models.json"),
        json!({"fetched_at": shadowcode_core::now(), "models": [{
            "id":"acme/coder","name":"Acme Coder","context_length":65536,
            "prompt_price":0.000001,"completion_price":0.000004,"tools":true,"vision":false
        }]})
        .to_string(),
    )
    .unwrap();
    let estimate = call(
        &service,
        "GET",
        &format!("/api/spending/estimate?session_id={session}&model=api:openrouter:acme/coder"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(estimate["show"], true, "{estimate}");
    let (low, high) = (
        estimate["low_usd"].as_f64().unwrap(),
        estimate["high_usd"].as_f64().unwrap(),
    );
    assert!(low > 0.0 && high > low, "{estimate}");
    assert!(estimate["label"].as_str().unwrap().starts_with("about $"));
    let unpriced = call(
        &service,
        "GET",
        "/api/spending/estimate?model=api:openrouter:unknown/model",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(unpriced, json!({"show":false,"reason":"no_prices"}));
    let subscription = call(
        &service,
        "GET",
        "/api/spending/estimate?model=cli:codex",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(subscription["reason"], "not_paid");
    service.engine.shutdown().await.unwrap();
    drop(root);
}

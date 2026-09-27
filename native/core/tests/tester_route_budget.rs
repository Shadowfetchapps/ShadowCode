//! Measure the 4096-token tester-route budget. Do not raise the limit to
//! hide overflow: record the actual message/schema/reserve components.
//!
//! 4096 is not too small by design for this fixture. Builtin schemas plus a
//! short tester turn fit the hard reserve. Compact used to insert a keep-list
//! note that could push a fitting request over 4096 when the workspace path
//! (or keep-list) was only slightly larger — the parallel-load flake.
mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    autonomy,
    config::Config,
    context,
    paths::AppPaths,
    service::{Request, Service},
    tools,
    workspace::Workspace,
};
use std::{fs, time::Duration};

fn tester_messages(workspace: &Workspace) -> Vec<Value> {
    let mut system = context::system(workspace, "code");
    system.push_str("\n\nGoal milestone checklist:\n- Explain (done)\n- Accept (pending)\n\nWork on the current milestone toward the original goal. Preserve earlier completed work. Report concrete evidence and unresolved limitations. Run the relevant acceptance checks with the terminal. This milestone requires a successful recorded command; a prose assertion alone cannot complete it. If checks fail or cannot run, report that honestly.");
    vec![
        json!({"role":"system","content":system}),
        json!({"role":"user","content":"Explain\n\nGoal: Explain the result, then run acceptance"}),
        json!({"role":"assistant","content":"Implementation milestone complete"}),
        json!({"role":"user","content":"Accept\n\nGoal: Explain the result, then run acceptance"}),
    ]
}

fn needed(messages: &[Value], schemas: &[Value]) -> usize {
    context::estimate_tokens(&json!(messages)) + context::estimate_tokens(&json!(schemas)) + 512
}

fn workspace_at(path: &std::path::Path) -> Workspace {
    fs::create_dir_all(path).unwrap();
    Workspace::open(path).unwrap()
}

/// Size the path against today's catalog, keeping the same 4096-token limit
/// and 512-token combined protocol/response reserve as the actual request.
/// A fixed 400-byte path stops being a fitting fixture when tools are added.
fn long_project(root: &std::path::Path) -> std::path::PathBuf {
    let workspace = workspace_at(root);
    let base = needed(&tester_messages(&workspace), &tools::schemas());
    assert!(
        base < 4080,
        "Short tester request must leave room for path pressure"
    );
    let padding = (4088 - base) * 3;
    root.join("p".repeat(padding.min(200)))
        .join("q".repeat(padding.saturating_sub(200).clamp(1, 200)))
}

#[test]
fn tester_route_4096_components_are_measurable() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let workspace = Workspace::open(&project).unwrap();
    let messages = tester_messages(&workspace);
    let schemas = tools::schemas();
    let message_tokens = context::estimate_tokens(&json!(messages));
    let schema_tokens = context::estimate_tokens(&json!(schemas));
    let reserved_min = 512;
    let reserved_output_quarter = 4096 / 4;
    let compact_hard = 4096usize.saturating_sub(reserved_output_quarter + schema_tokens + 256);
    let hard_needed = needed(&messages, &schemas);
    let budget = autonomy::account(&messages, &schemas, 4096).unwrap();
    eprintln!(
        "tester_route_components path_len={} messages={} schemas={} needed={} compact_hard={} account_used={} remaining={} fits={}",
        workspace.path.display().to_string().len(),
        message_tokens,
        schema_tokens,
        hard_needed,
        compact_hard,
        budget["used_estimated_tokens"],
        budget["remaining"],
        budget["fits"]
    );
    assert!(
        schema_tokens + reserved_min < 4096,
        "4096 is too small for the builtin tool catalog alone: schemas={schema_tokens}"
    );
    assert!(
        hard_needed <= 4096,
        "live tester request already exceeds 4096 before compact: needed={hard_needed} messages={message_tokens} schemas={schema_tokens}"
    );
}

#[test]
fn compact_must_not_inflate_a_fitting_4096_tester_request() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let workspace = Workspace::open(&project).unwrap();
    let mut messages = tester_messages(&workspace);
    let schemas = tools::schemas();
    let before = needed(&messages, &schemas);
    assert!(
        before <= 4096,
        "precondition: tester request must fit the hard reserve (needed={before})"
    );
    let compacted = context::compact(&mut messages, &schemas, 4096, 0.55);
    let after = needed(&messages, &schemas);
    eprintln!(
        "tester_route_compact before={before} after={after} result={:?}",
        compacted
            .as_ref()
            .map(|r| r.as_ref().map(|v| v["omitted_messages"].clone()))
    );
    assert!(
        compacted.is_ok(),
        "compact failed a request that already fit 4096: {compacted:?} before={before} after={after}"
    );
    assert!(
        after <= 4096,
        "compact inflated a fitting 4096 tester request: before={before} after={after}"
    );
    context::validate_pairs(&messages).unwrap();
    assert_eq!(
        messages.last().unwrap()["content"],
        "Accept\n\nGoal: Explain the result, then run acceptance"
    );
}

#[test]
fn compact_does_not_fail_a_fitting_4096_request_when_the_workspace_path_is_long() {
    let root = tempfile::tempdir().unwrap();
    // Keep the live request close to 4096 before testing compaction.
    let project = long_project(root.path());
    let workspace = workspace_at(&project);
    let mut messages = tester_messages(&workspace);
    let schemas = tools::schemas();
    let before = needed(&messages, &schemas);
    assert!(
        before <= 4096,
        "precondition failed: long path itself exceeded 4096 (needed={before})"
    );
    context::compact(&mut messages, &schemas, 4096, 0.55)
        .expect("compact must not fail a request that already fit 4096");
    let after = needed(&messages, &schemas);
    assert!(
        after <= 4096,
        "compact/keep-list pushed a fitting 4096 tester request over the limit: before={before} after={after}"
    );
    assert_eq!(
        messages.last().unwrap()["content"],
        "Accept\n\nGoal: Explain the result, then run acceptance"
    );
}

#[test]
fn request_that_already_exceeds_4096_still_fails() {
    let mut messages = vec![
        json!({"role":"system","content":"x".repeat(16000)}),
        json!({"role":"user","content":"Keep my complete current request"}),
    ];
    assert!(context::compact(&mut messages, &[], 4096, 0.7).is_err());
}

fn response(text: &str, calls: Value) -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":if calls.as_array().is_some_and(|v|!v.is_empty()){"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30}})
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
async fn goal_verification_on_4096_tester_survives_a_long_workspace_path() {
    const CHECK: &str = "test \"$(cat acceptance.txt)\" = verified";
    let primary = support::server(|_, _| {
        (
            response("Implementation milestone complete", json!([])),
            Duration::ZERO,
        )
    })
    .await;
    let tester = support::server(
        |index, _| {
            (
                if index == 0 {
                    response(
                        "Running acceptance",
                        json!([{
                            "id":"acceptance-write",
                            "type":"function",
                            "function":{
                                "name":"exec",
                                "arguments":json!({"command":"printf verified > acceptance.txt"}).to_string()
                            }
                        }, {
                            "id":"acceptance-check",
                            "type":"function",
                            "function":{
                                "name":"exec",
                                "arguments":json!({"command":CHECK}).to_string()
                            }
                        }]),
                    )
                } else {
                    response("Acceptance passed", json!([]))
                },
                Duration::ZERO,
            )
        },
    )
    .await;
    let root = tempfile::tempdir().unwrap();
    let project = long_project(root.path());
    fs::create_dir_all(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"model":{"default":"primary","name":"shared","provider":"local","endpoint":primary.endpoint,"context_limit":8192},"trusted_workspaces":[&project],"permissions":{"approve_shell":false},"agent":{"retry_attempts":0},"verification":{"commands":[CHECK]}})).unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    call(
        &service,
        "POST",
        "/api/models/register",
        json!({"id":"tester-model","name":"shared","provider":"local","endpoint":tester.endpoint,"context_limit":4096}),
    )
    .await
    .unwrap();
    call(
        &service,
        "PUT",
        "/api/routing",
        json!({"values":{"enabled":true,"tester":"tester-model"}}),
    )
    .await
    .unwrap();
    let goal = call(
        &service,
        "POST",
        "/api/goals",
        json!({"instruction":"Explain the result, then run acceptance","milestones":[{"title":"Explain","mode":"code"},{"title":"Accept","mode":"code","require_verification":true}],"run":true}),
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        service.engine.wait_goal(goal["id"].as_str().unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["status"], "completed", "{result}");
    let jobs = service.engine.store().jobs(10).unwrap();
    let tester_job = jobs
        .iter()
        .find(|job| job["routing"]["model_id"] == "tester-model")
        .expect("tester route executed");
    let verification = &tester_job["result"]["verification"];
    assert_eq!(verification["status"], "passed");
    let check = verification["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|receipt| receipt["command"] == CHECK)
        .expect("configured acceptance command recorded");
    assert_eq!(check["kind"], "configured_check");
    assert_eq!(check["state"], "passed");
    assert_eq!(check["exit_code"], 0);
    assert_eq!(check["provenance"], "locally_observed");
    assert_eq!(check["task_id"], tester_job["task_id"]);
    assert_eq!(
        fs::read_to_string(project.join("acceptance.txt")).unwrap(),
        "verified"
    );
    let events = service
        .engine
        .store()
        .events_after(result["session_id"].as_str().unwrap(), 0, None, 200)
        .unwrap();
    for event in &events {
        if event["type"] == "context.budget" {
            eprintln!(
                "live_tester_budget used={} remaining={} fits={} limit={}",
                event["payload"]["used_estimated_tokens"],
                event["payload"]["remaining"],
                event["payload"]["fits"],
                event["payload"]["limit"]
            );
        }
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "context.budget"
                && e["payload"]["limit"] == 4096
                && e["payload"]["fits"] == false)
            .count(),
        0,
        "4096 tester route reported a non-fitting budget"
    );
    assert_eq!(primary.requests.lock().unwrap().len(), 1);
    assert_eq!(tester.requests.lock().unwrap().len(), 2);
    service.engine.shutdown().await.unwrap();
}

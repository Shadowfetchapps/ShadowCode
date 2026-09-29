//! Tokens and cost a vendor CLI reports are counted exactly once: kept when
//! the turn fails, hits the plan limit or is cancelled, and never counted
//! twice when a Codex thread is resumed. The vendor is a fake Codex
//! app-server (plus the recorded Codex 0.158 transcripts); no real vendor runs.
mod vendor_support;
use serde_json::{json, Value};
use shadowcode_core::{
    cli_agent::{adapter_for, LaunchOptions, Update, Vendor},
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use vendor_support::{cli_agents, eventually, FakeCodex};

struct Setup {
    _root: tempfile::TempDir,
    fake: FakeCodex,
    service: Service,
}

fn setup(fake_config: Value) -> Setup {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let fake = FakeCodex::new(root.path(), fake_config);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({
            "model":{"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"fixture","context_limit":16384},
            "trusted_workspaces":[project],
            "cli_agents": cli_agents(&fake),
        }),
    )
    .unwrap();
    let service = Service::open(paths, Some(project)).unwrap();
    Setup {
        _root: root,
        fake,
        service,
    }
}

async fn call(service: &Service, method: &str, path: &str, body: Value) -> Value {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
        .unwrap_or_else(|e| panic!("{method} {path}: {e:#}"))
}

async fn finished(service: &Service, id: &str) -> Value {
    let done = tokio::time::timeout(Duration::from_secs(40), service.engine.wait(id))
        .await
        .expect("job did not finish")
        .unwrap();
    json!(done)
}

fn session_usage(service: &Service, sid: &str) -> Value {
    let session = service.engine.store().session(sid).unwrap().unwrap();
    serde_json::from_str(session["usage_json"].as_str().unwrap()).unwrap()
}

/// The fake's `usage_first` report: 1200 input (1000 cached), 34 output.
fn assert_reported_usage_kept(service: &Service, done: &Value) {
    let usage = &done["usage"];
    assert_eq!(usage["prompt_tokens"], 1200, "{done}");
    assert_eq!(usage["completion_tokens"], 34, "{done}");
    assert_eq!(usage["cached_tokens"], 1000, "{done}");
    assert_eq!(usage["source"], "vendor", "{done}");
    assert_eq!(
        done["usage_is_estimated"], false,
        "the vendor reported real counts"
    );
    assert_eq!(
        done["result"]["usage"], *usage,
        "the saved result carries the same counts"
    );
    let sid = done["session_id"].as_str().unwrap();
    let total = session_usage(service, sid);
    assert_eq!(total["prompt_tokens"], 1200, "session total: {total}");
    assert_eq!(total["completion_tokens"], 34, "session total: {total}");
    let events = service
        .engine
        .store()
        .events_after(sid, 0, None, 10_000)
        .unwrap();
    assert!(
        events.iter().any(|e| e["type"] == "usage.updated"
            && e["task_id"] == done["task_id"]
            && e["payload"]["turn"]["prompt_tokens"] == 1200),
        "the window is told about the counted tokens"
    );
}

#[tokio::test]
async fn failed_vendor_turn_keeps_the_tokens_it_reported() {
    let setup = setup(json!({"auth":"chatgpt","turn":"fail","usage_first":true}));
    let job = call(
        &setup.service,
        "POST",
        "/api/jobs",
        json!({"task":"fix the build","model":"cli:codex"}),
    )
    .await;
    let done = finished(&setup.service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "failed", "{done}");
    assert_reported_usage_kept(&setup.service, &done);
}

#[tokio::test]
async fn plan_limit_keeps_the_tokens_the_turn_already_used() {
    let setup = setup(json!({"auth":"chatgpt","turn":"limit","usage_first":true}));
    let job = call(
        &setup.service,
        "POST",
        "/api/jobs",
        json!({"task":"big refactor","model":"cli:codex"}),
    )
    .await;
    let done = finished(&setup.service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "limit_reached", "{done}");
    assert_eq!(done["result"]["limit_reached"]["vendor"], "codex");
    assert_reported_usage_kept(&setup.service, &done);
}

#[tokio::test]
async fn cancelled_vendor_turn_keeps_the_tokens_it_reported() {
    let setup = setup(json!({"auth":"chatgpt","turn":"usage_then_wait","usage_first":true}));
    let service = &setup.service;
    let job = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"long job","model":"cli:codex"}),
    )
    .await;
    let id = job["id"].as_str().unwrap().to_owned();
    let sid = job["session_id"].as_str().unwrap().to_owned();
    // The fake reports usage, then streams text. Once that text is saved the
    // runner has read the usage line before it.
    eventually(
        || {
            let events = service
                .engine
                .store()
                .events_after(&sid, 0, None, 10_000)
                .ok()?;
            events
                .iter()
                .any(|e| {
                    matches!(e["type"].as_str(), Some("model.stream" | "model.delta"))
                        && e["payload"]["text"]
                            .as_str()
                            .is_some_and(|t| t.contains("working on it"))
                })
                .then_some(())
        },
        "the streamed partial reply",
    )
    .await;
    assert!(setup.fake.marker("usage_sent").is_some());
    call(
        service,
        "POST",
        &format!("/api/jobs/{id}/cancel"),
        json!({}),
    )
    .await;
    let done = finished(service, &id).await;
    assert_eq!(done["status"], "cancelled", "{done}");
    assert_reported_usage_kept(service, &done);
}

/// Unconfirmed report: resumed Codex turns double count. Each turn adds only
/// its own `last` counts. The thread's cumulative `total` and the usage Codex
/// repeats for the previous turn right after `thread/resume` are not added.
#[tokio::test]
async fn resumed_codex_turns_count_each_turn_once() {
    let setup = setup(json!({"auth":"chatgpt","resume_replays_usage":true}));
    let service = &setup.service;
    let first = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"first","model":"cli:codex"}),
    )
    .await;
    let first = finished(service, first["id"].as_str().unwrap()).await;
    assert_eq!(first["status"], "completed", "{first}");
    let sid = first["session_id"].as_str().unwrap().to_owned();
    let second = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"second","model":"cli:codex","session_id":sid}),
    )
    .await;
    let second = finished(service, second["id"].as_str().unwrap()).await;
    assert_eq!(second["status"], "completed", "{second}");
    let threads = setup.fake.marker("threads.log").unwrap();
    assert!(
        threads
            .lines()
            .nth(1)
            .is_some_and(|l| l.starts_with("thread/resume")),
        "the second turn resumed the Codex thread: {threads}"
    );
    // The fake reports two model calls per turn, each last={10,5} with a
    // cumulative total of {1000n,500n}.
    for done in [&first, &second] {
        assert_eq!(done["usage"]["prompt_tokens"], 20, "{done}");
        assert_eq!(done["usage"]["completion_tokens"], 10, "{done}");
    }
    let total = session_usage(service, &sid);
    assert_eq!(total["prompt_tokens"], 40, "{total}");
    assert_eq!(total["completion_tokens"], 10 + 10, "{total}");
    assert_eq!(total["total_tokens"], 60, "{total}");
}

fn fixture(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/vendors")
        .join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A recorded Codex 0.158 turn with two model calls (an approval between
/// them): the per-call counts add up to exactly the thread's final total.
#[test]
fn recorded_codex_turn_with_two_model_calls_counts_each_call_once() {
    let mut adapter = adapter_for(Vendor::Codex, false);
    adapter.on_start(&LaunchOptions {
        binary: "vendor".into(),
        workspace: PathBuf::from("/tmp/shadowcode-fixture/project"),
        model: "gpt-6-luna".into(),
        ..Default::default()
    });
    adapter.prompt("fixture", &[]).unwrap();
    let mut usage = Vec::new();
    for line in fixture("codex-0.158.0-approval.jsonl") {
        let step = adapter.on_line(&line).unwrap();
        for update in step.updates {
            match update {
                Update::Approval(prompt) => {
                    adapter.approve(&prompt.request_id, true).unwrap();
                }
                Update::Usage {
                    input,
                    output,
                    cached,
                } => usage.push((input, output, cached)),
                _ => {}
            }
        }
    }
    assert_eq!(usage, [(17675, 107, 7936), (17806, 5, 17152)]);
    let sum = usage
        .iter()
        .fold((0, 0, 0), |a, u| (a.0 + u.0, a.1 + u.1, a.2 + u.2));
    // The recorded final `total` for the thread.
    assert_eq!(sum, (35481, 112, 25088));
}

//! Roles: a model or vendor CLI per role (plan, implement, review, explore).
//! Plan → Implement → Review tasks with vendor roles on a fake Codex
//! app-server and a review role on a fake OpenAI-compatible model on this
//! computer; consent before a local conversation's work reaches a cloud role;
//! approvals, cancellation and usage of vendor children; the diff applied
//! with the usual approval; subagents routed to their role's model. No real
//! vendor CLI or model runs.
#![cfg(unix)]
mod vendor_support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    roles,
    service::{Request, Service},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use vendor_support::{cli_agents, eventually, FakeCodex};

type Handler = dyn Fn(&Value) -> Value + Send + Sync;

/// A fake chat-completions server on loopback (so it runs "on this
/// computer"); it answers connections concurrently and records requests.
struct Model {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    worker: tokio::task::JoinHandle<()>,
}
impl Drop for Model {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
async fn model(handler: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Model {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let handler: Arc<Handler> = Arc::new(handler);
    let captured = requests.clone();
    let worker = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let (handler, captured) = (handler.clone(), captured.clone());
            tokio::spawn(async move {
                let mut wire = Vec::new();
                let mut buffer = [0; 8192];
                let body = loop {
                    let count = socket.read(&mut buffer).await.unwrap_or(0);
                    if count == 0 {
                        return;
                    }
                    wire.extend_from_slice(&buffer[..count]);
                    if let Some(end) = wire.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&wire[..end]).to_lowercase();
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if wire.len() >= end + 4 + len {
                            break serde_json::from_slice::<Value>(&wire[end + 4..end + 4 + len])
                                .unwrap();
                        }
                    }
                };
                captured.lock().unwrap().push(body.clone());
                let text = handler(&body).to_string();
                let _ = socket
                    .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len()).as_bytes())
                    .await;
            });
        }
    });
    Model {
        endpoint,
        requests,
        worker,
    }
}

fn answer(text: &str, calls: Value) -> Value {
    let reason = if calls.as_array().is_some_and(|v| !v.is_empty()) {
        "tool_calls"
    } else {
        "stop"
    };
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":reason}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}})
}
fn tool(name: &str, args: Value) -> Value {
    json!({"id":shadowcode_core::id(),"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn system(body: &Value) -> String {
    body["messages"][0]["content"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}
fn last(body: &Value) -> Value {
    body["messages"].as_array().unwrap().last().unwrap().clone()
}
fn last_user(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_owned()
}
fn tool_output(message: &Value) -> Value {
    serde_json::from_str(message["content"].as_str().unwrap_or("{}")).unwrap_or(Value::Null)
}

struct Setup {
    _root: tempfile::TempDir,
    paths: AppPaths,
    project: PathBuf,
    fake: FakeCodex,
    service: Service,
    _model: Model,
    requests: Arc<Mutex<Vec<Value>>>,
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
}

/// A Git project, a fake Codex and a conversation model on this computer.
async fn setup(
    fake_config: Value,
    extra: Value,
    handler: impl Fn(&Value) -> Value + Send + Sync + 'static,
) -> Setup {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let project = project.canonicalize().unwrap();
    git(&project, &["init", "-q"]);
    fs::write(project.join("note.txt"), "old\n").unwrap();
    git(&project, &["add", "note.txt"]);
    git(&project, &["commit", "-q", "-m", "init"]);
    let fake = FakeCodex::new(root.path(), fake_config);
    let model = model(handler).await;
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let mut config = json!({
        "model":{"provider":"local","endpoint":model.endpoint,"name":"fixture","context_limit":32768},
        "trusted_workspaces":[project],
        "cli_agents": cli_agents(&fake),
        "permissions":{"approve_shell":false},
        "agent":{"max_steps":12,"model_retries":0},
    });
    shadowcode_core::config::merge(&mut config, extra);
    Config::patch(&paths, config).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    let requests = model.requests.clone();
    Setup {
        _root: root,
        paths,
        project,
        fake,
        service,
        _model: model,
        requests,
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
async fn refused(service: &Service, method: &str, path: &str, body: Value) -> String {
    match service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
    {
        Ok(value) => panic!("{method} {path} should fail, got {value}"),
        Err(error) => format!("{error:#}"),
    }
}
async fn finished(service: &Service, id: &str) -> Value {
    match tokio::time::timeout(Duration::from_secs(90), service.engine.wait(id)).await {
        Ok(done) => json!(done.unwrap()),
        Err(_) => {
            // Show what happened before failing.
            let job = service.engine.job(id).unwrap().unwrap();
            for event in service
                .engine
                .store()
                .events_after(&job.session_id, 0, None, 10_000)
                .unwrap()
            {
                eprintln!("{} {}", event["type"], event["payload"]);
            }
            panic!("job {id} did not finish");
        }
    }
}
fn events(service: &Service, sid: &str, kind: &str) -> Vec<Value> {
    service
        .engine
        .store()
        .events_after(sid, 0, None, 10_000)
        .unwrap()
        .into_iter()
        .filter(|e| e["type"] == kind)
        .collect()
}
fn lines(fake: &FakeCodex, name: &str) -> Vec<Value> {
    fake.marker(name)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or(json!(l)))
        .collect()
}

#[tokio::test]
async fn roles_are_saved_per_project_with_presets_and_resolved_for_the_conversation() {
    let setup = setup(json!({"auth":"chatgpt"}), json!({}), |_| {
        answer("ok", json!([]))
    })
    .await;
    let service = &setup.service;
    let view = call(service, "GET", "/api/roles", json!(null)).await;
    assert_eq!(view["setup"]["pipeline"], false);
    assert_eq!(view["workspace"], json!(setup.project));
    assert_eq!(view["conversation"]["local"], true);
    for role in ["plan", "implement", "review", "explore"] {
        // Empty roles use the conversation's model.
        assert_eq!(view["roles"][role]["name"], "fixture", "{role}");
        assert_eq!(view["roles"][role]["cost"], "local", "{role}");
        assert_eq!(view["roles"][role]["runner"], "shadowcode", "{role}");
    }
    assert!(view["presets"].as_array().unwrap().len() >= 3);

    let view = call(
        service,
        "POST",
        "/api/roles",
        json!({"preset":"claude-plans"}),
    )
    .await;
    assert_eq!(view["setup"]["plan"], "cli:claude");
    assert_eq!(view["setup"]["review"], roles::SKIP);
    assert_eq!(view["setup"]["preset"], "claude-plans");
    assert_eq!(view["roles"]["plan"]["name"], "Claude Code");
    assert_eq!(view["roles"]["plan"]["runner"], "vendor");
    assert_eq!(view["roles"]["plan"]["cost"], "subscription");
    // A local conversation asks before a cloud role receives its work.
    assert_eq!(view["roles"]["plan"]["needs_consent"], true);
    assert_eq!(view["roles"]["review"]["skipped"], true);

    // A preset with a local role needs a local model; none is installed.
    let error = refused(
        service,
        "POST",
        "/api/roles",
        json!({"preset":"claude-codex-local"}),
    )
    .await;
    assert!(
        error.contains("No model on this computer is ready"),
        "{error}"
    );

    // One role changed by hand: the preset label is cleared.
    let view = call(
        service,
        "POST",
        "/api/roles",
        json!({"implement":"cli:codex:gpt-5.6-luna","pipeline":true}),
    )
    .await;
    assert_eq!(view["setup"]["implement"], "cli:codex:gpt-5.6-luna");
    assert_eq!(view["setup"]["pipeline"], true);
    assert_eq!(view["setup"]["preset"], "");
    assert_eq!(view["roles"]["implement"]["name"], "Codex · gpt-5.6-luna");
    let error = refused(service, "POST", "/api/roles", json!({"implement":"skip"})).await;
    assert!(error.contains("cannot be skipped"), "{error}");
    let error = refused(
        service,
        "POST",
        "/api/roles",
        json!({"review":"no-such-model"}),
    )
    .await;
    assert!(error.contains("review role's model"), "{error}");
    // Stored by ShadowCode, not in the repository.
    assert!(!setup.project.join(".shadow").exists());
    let stored = roles::load(&service.engine.store(), &setup.project).unwrap();
    assert_eq!(stored.implement, "cli:codex:gpt-5.6-luna");

    // Offline, cloud roles are refused with the reason.
    Config::patch(&setup.paths, json!({"network":{"mode":"offline"}})).unwrap();
    let view = call(service, "GET", "/api/roles", json!(null)).await;
    let blocked = view["roles"]["plan"]["blocked"].as_str().unwrap();
    assert!(blocked.contains("Offline mode"), "{blocked}");
    assert!(view["roles"]["review"]["blocked"].is_null());
    let error = refused(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"Add a test","roles":true}),
    )
    .await;
    assert!(
        error.contains("Offline mode: the plan role uses Claude Code"),
        "{error}"
    );
    assert!(
        service.engine.store().jobs(10).unwrap().is_empty(),
        "nothing started"
    );
}

/// The review role on the conversation's local model: checks it received
/// the request, the plan and the diff, then approves.
fn reviewer(body: &Value) -> Value {
    let task = last_user(body);
    assert!(
        system(body).contains("review role of a Plan → Implement → Review task"),
        "{}",
        system(body)
    );
    assert!(
        task.contains("<request>\nUpdate the note\n</request>"),
        "{task}"
    );
    assert!(task.contains("<plan from=\"Codex\">"), "{task}");
    assert!(task.contains("1. Change note.txt to say new"), "{task}");
    assert!(task.contains("+new"), "{task}");
    assert!(task.contains("added.txt"), "{task}");
    answer("The note now says new.\nVerdict: ready", json!([]))
}

#[tokio::test]
async fn plan_implement_review_runs_vendor_roles_and_applies_the_reviewed_diff() {
    let setup = setup(
        json!({
            "auth":"chatgpt",
            "turn":"ok",
            "mode_when":{"implement role":"edit"},
            "edits":{"note.txt":"new\n","added.txt":"hello\n"},
            "replies":{"plan role":"1. Change note.txt to say new.\n2. Add added.txt."},
        }),
        json!({"permissions":{"mode":"ask"}}),
        reviewer,
    )
    .await;
    let service = &setup.service;
    call(
        service,
        "POST",
        "/api/roles",
        json!({"plan":"cli:codex","implement":"cli:codex","review":"","pipeline":true}),
    )
    .await;
    let session = call(service, "POST", "/api/sessions", json!({})).await;
    let sid = session["id"].as_str().unwrap().to_owned();
    let body = json!({"task":"Update the note","session_id":sid,"roles":true});
    // This conversation runs on this computer: Codex needs consent first.
    let ask = call(service, "POST", "/api/jobs", body.clone()).await;
    assert_eq!(ask["needs_consent"], true, "{ask}");
    let reason = ask["handoff"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("the plan role (Codex) and the implement role (Codex)"),
        "{reason}"
    );
    assert_eq!(ask["handoff"]["to"], "Codex");
    assert_eq!(ask["handoff"]["roles"].as_array().unwrap().len(), 2);
    assert!(service
        .engine
        .store()
        .session_jobs(&sid, 5)
        .unwrap()
        .is_empty());
    assert!(setup.fake.marker("prompts.log").is_none(), "nothing ran");

    let mut consented = body.clone();
    consented["handoff_consent"] = json!(true);
    let job = call(service, "POST", "/api/jobs", consented).await;
    assert!(
        job["model"]
            .as_str()
            .unwrap()
            .starts_with("Roles: Codex → Codex → fixture"),
        "{job}"
    );
    assert_eq!(job["routing"]["provider"], roles::PROVIDER);
    let job_id = job["id"].as_str().unwrap().to_owned();
    let task_id = job["task_id"].as_str().unwrap().to_owned();
    // The diff is applied with the usual edit approval, in this conversation.
    let approval = eventually(
        || {
            service
                .engine
                .approvals()
                .list(Some(sid.as_str()))
                .into_iter()
                .find(|a| a.task_id == task_id)
        },
        "the apply approval",
    )
    .await;
    assert_eq!(approval.tool, "apply_patch");
    assert_eq!(
        fs::read_to_string(setup.project.join("note.txt")).unwrap(),
        "old\n"
    );
    service
        .engine
        .approvals()
        .decide(&approval.id, &sid, true)
        .unwrap();
    let done = finished(service, &job_id).await;
    assert_eq!(done["status"], "completed", "{}", done["summary"]);
    assert_eq!(
        fs::read_to_string(setup.project.join("note.txt")).unwrap(),
        "new\n"
    );
    assert_eq!(
        fs::read_to_string(setup.project.join("added.txt")).unwrap(),
        "hello\n"
    );
    let summary = done["summary"].as_str().unwrap();
    assert!(summary.contains("**Plan** · Codex — done"), "{summary}");
    assert!(
        summary.contains("**Implement** · Codex — done, 2 files changed"),
        "{summary}"
    );
    assert!(
        summary.contains("**Review** · fixture — done, verdict: ready"),
        "{summary}"
    );
    assert!(
        summary.contains("The changes were applied to the project."),
        "{summary}"
    );

    // Role cards: who did what, on which runner, at what cost.
    let runs: Vec<Value> = events(service, &sid, "subagent.finished")
        .into_iter()
        .map(|e| e["payload"].clone())
        .collect();
    assert_eq!(runs.len(), 3);
    let roles_seen: Vec<&str> = runs.iter().map(|r| r["role"].as_str().unwrap()).collect();
    assert_eq!(roles_seen, ["plan", "implement", "review"]);
    assert_eq!(runs[0]["runner"], "vendor");
    assert_eq!(runs[0]["vendor"], "codex");
    assert_eq!(runs[0]["route"], "cloud");
    assert_eq!(runs[0]["cost"], "subscription");
    assert_eq!(runs[0]["mode"], "read-only");
    assert_eq!(runs[1]["mode"], "write");
    assert_eq!(runs[1]["patch"], true);
    assert_eq!(runs[2]["runner"], "shadowcode");
    assert_eq!(runs[2]["cost"], "local");
    assert_eq!(runs[2]["verdict"], "ready");
    assert!(runs.iter().all(|r| r["status"] == "completed"));
    let applied = events(service, &sid, "subagent.applied");
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0]["payload"]["run_id"], runs[1]["run_id"]);
    let summary = events(service, &sid, "roles.finished");
    assert_eq!(summary.len(), 1);
    let payload = &summary[0]["payload"];
    assert_eq!(payload["applied"], true);
    assert_eq!(payload["completed"], true);
    assert_eq!(payload["stages"].as_array().unwrap().len(), 3);
    assert_eq!(payload["stages"][1]["files"], 2);
    assert_eq!(payload["stages"][2]["verdict"], "ready");

    // The plan ran read-only in the project; the implementation in its own
    // worktree, which is gone afterwards.
    let sandboxes = lines(&setup.fake, "sandbox.log");
    assert_eq!(sandboxes.len(), 2, "{sandboxes:?}");
    assert_eq!(sandboxes[0]["sandbox"], "read-only");
    assert_eq!(sandboxes[0]["cwd"], json!(setup.project));
    assert_eq!(sandboxes[1]["sandbox"], "workspace-write");
    let worktree = sandboxes[1]["cwd"].as_str().unwrap();
    assert_ne!(worktree, setup.project.to_str().unwrap());
    assert!(!Path::new(worktree).exists(), "the worktree was removed");
    // Handoff: the implement role received the plan.
    let prompts = lines(&setup.fake, "prompts.log");
    assert_eq!(prompts.len(), 2);
    let plan_prompt = prompts[0].as_str().unwrap();
    assert!(plan_prompt.contains("plan role of a Plan → Implement → Review task"));
    assert!(plan_prompt.contains("<request>\nUpdate the note\n</request>"));
    let implement_prompt = prompts[1].as_str().unwrap();
    assert!(implement_prompt.contains("<plan from=\"Codex\">\n1. Change note.txt to say new."));

    // Usage of every role counts toward the task.
    let run_tokens: u64 = runs
        .iter()
        .map(|r| r["usage"]["total_tokens"].as_u64().unwrap_or(0))
        .sum();
    assert!(run_tokens > 0);
    assert_eq!(done["usage"]["total_tokens"].as_u64().unwrap(), run_tokens);
    // Children are hidden conversations of this one.
    for run in &runs {
        let child = run["session_id"].as_str().unwrap();
        let meta = service
            .engine
            .store()
            .session_meta(child, "subagent_parent")
            .unwrap();
        assert_eq!(meta.as_deref(), Some(sid.as_str()));
    }
    // Consent is remembered for this conversation: the next task starts.
    let stored = roles::consented(&service.engine.store(), &sid);
    assert!(stored.contains("cli:codex"), "{stored:?}");
    let next = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"Plan the next step","purpose":"planner","session_id":sid,"roles":true}),
    )
    .await;
    assert!(next["needs_consent"].is_null(), "{next}");
    let next = finished(service, next["id"].as_str().unwrap()).await;
    assert_eq!(next["status"], "completed", "{}", next["summary"]);
    // A Plan task runs the plan role only and answers with the plan.
    assert!(next["summary"]
        .as_str()
        .unwrap()
        .contains("1. Change note.txt to say new."));
    // A later single-model turn receives the roles' work as a handoff.
    let tape = service
        .engine
        .store()
        .latest_session_messages(&sid, "none")
        .unwrap();
    assert!(tape.iter().any(|m| m["content"]
        .as_str()
        .is_some_and(|c| c.contains("[Roles: Codex → Codex → fixture]"))));
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_vendor_roles_approvals_reach_the_parent_and_stop_ends_it() {
    let setup = setup(
        json!({"auth":"chatgpt","turn":"ok","mode_when":{"implement role":"approval","plan role":"slow"},"slow":30}),
        json!({"permissions":{"approve_shell":true}}),
        |_| answer("unused", json!([])),
    )
    .await;
    let service = &setup.service;
    // A cloud conversation (Codex) needs no consent for its roles.
    call(
        service,
        "POST",
        "/api/roles",
        json!({"plan":roles::SKIP,"implement":"cli:codex","review":roles::SKIP,"pipeline":true}),
    )
    .await;
    let job = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"Clean the build","model":"cli:codex","roles":true}),
    )
    .await;
    assert!(job["needs_consent"].is_null(), "{job}");
    let sid = job["session_id"].as_str().unwrap().to_owned();
    let task_id = job["task_id"].as_str().unwrap().to_owned();
    let approval = eventually(
        || {
            service
                .engine
                .approvals()
                .list(Some(sid.as_str()))
                .into_iter()
                .next()
        },
        "the vendor role's approval",
    )
    .await;
    // Asked in the parent conversation, with the role's name.
    assert!(
        approval.reason.starts_with("Implement role (Codex):"),
        "{}",
        approval.reason
    );
    assert_eq!(approval.command, "rm -rf build");
    assert_ne!(approval.task_id, task_id, "the role's own task asked");
    service
        .engine
        .approvals()
        .decide(&approval.id, &sid, true)
        .unwrap();
    let done = finished(service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{}", done["summary"]);
    let summary = done["summary"].as_str().unwrap();
    assert!(summary.contains("no file changes"), "{summary}");
    assert!(
        summary.contains("The implement role made no changes."),
        "{summary}"
    );
    let runs = events(service, &sid, "subagent.finished");
    assert_eq!(runs.len(), 1);
    assert!(runs[0]["payload"]["summary"]
        .as_str()
        .unwrap()
        .contains("decision=accept"));

    // Stop: the running vendor role is stopped with its task.
    call(service, "POST", "/api/roles", json!({"plan":"cli:codex"})).await;
    let job = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"Plan a slow change","purpose":"planner","session_id":sid,"model":"cli:codex","roles":true}),
    )
    .await;
    let started = eventually(
        || {
            events(service, &sid, "subagent.started")
                .into_iter()
                .find(|e| e["payload"]["role"] == "plan")
        },
        "the plan role",
    )
    .await;
    let child_job = started["payload"]["job_id"].as_str().unwrap().to_owned();
    // The task orchestrates its roles: it is stopped, not paused or steered.
    let refused = service
        .engine
        .pause_job(job["id"].as_str().unwrap())
        .unwrap_err()
        .to_string();
    assert!(refused.contains("can't be paused or steered"), "{refused}");
    assert!(service
        .engine
        .steer_job(job["id"].as_str().unwrap(), "faster", None)
        .is_err());
    eventually(
        || {
            setup
                .fake
                .marker("prompts.log")
                .filter(|p| p.contains("plan role"))
        },
        "the plan prompt",
    )
    .await;
    let cancelled = tokio::time::timeout(
        Duration::from_secs(20),
        service.engine.cancel(job["id"].as_str().unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cancelled.status, "cancelled");
    let child = tokio::time::timeout(Duration::from_secs(20), service.engine.wait(&child_job))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child.status, "cancelled", "{}", child.summary);
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn subagents_use_their_roles_and_a_local_conversation_asks_before_a_cloud_role() {
    // The conversation's model spawns the plan role, then applies the
    // implement role's diff from its own worktree.
    let setup = setup(
        json!({
            "auth":"chatgpt",
            "turn":"ok",
            "mode_when":{"<task>\nEdit":"edit"},
            "edits":{"note.txt":"from codex\n"},
            "replies":{"subagent 'plan'":"Plan: edit note.txt"},
        }),
        json!({}),
        |body| {
            let message = last(body);
            if message["role"] == "tool" {
                let output = tool_output(&message);
                let text = output.to_string();
                if message["name"] == "spawn_agent" {
                    if text.contains("without your consent") {
                        return answer("The plan role needs consent.", json!([]));
                    }
                    let result = &output["output"];
                    if result["agent"] == "general" {
                        assert_eq!(result["role"], "implement", "{result}");
                        assert!(result["diff"].as_str().unwrap().contains("+from codex"));
                        let run = result["run_id"].as_str().unwrap();
                        return answer(
                            "",
                            json!([tool("apply_agent_changes", json!({"run_id":run}))]),
                        );
                    }
                    assert_eq!(result["role"], "plan", "{result}");
                    assert_eq!(result["model"], "Codex");
                    assert!(result["summary"]
                        .as_str()
                        .unwrap()
                        .contains("Plan: edit note.txt"));
                    return answer(
                        "",
                        json!([tool(
                            "spawn_agent",
                            json!({"agent":"general","prompt":"Edit note.txt"})
                        )]),
                    );
                }
                assert_eq!(output["success"], true, "{output}");
                return answer("Applied.", json!([]));
            }
            answer(
                "",
                json!([tool(
                    "spawn_agent",
                    json!({"agent":"plan","prompt":"Plan the edit"})
                )]),
            )
        },
    )
    .await;
    let service = &setup.service;
    call(
        service,
        "POST",
        "/api/roles",
        json!({"plan":"cli:codex","implement":"cli:codex"}),
    )
    .await;
    // Without consent, the model's own spawn is refused with the reason.
    let job = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"Change the note"}),
    )
    .await;
    let sid = job["session_id"].as_str().unwrap().to_owned();
    let done = finished(service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{}", done["summary"]);
    assert!(
        setup.fake.marker("prompts.log").is_none(),
        "Codex never ran"
    );
    assert_eq!(
        fs::read_to_string(setup.project.join("note.txt")).unwrap(),
        "old\n"
    );
    // An @plan request asks first, like any turn that moves work to the cloud.
    let body = json!({"task":"@plan the edit","session_id":sid});
    let ask = call(service, "POST", "/api/jobs", body.clone()).await;
    assert_eq!(ask["needs_consent"], true, "{ask}");
    assert_eq!(ask["handoff"]["to"], "Codex");
    assert!(ask["handoff"]["reason"]
        .as_str()
        .unwrap()
        .contains("the @plan subagent (the plan role) runs on Codex"));
    let mut consented = body;
    consented["handoff_consent"] = json!(true);
    let job = call(service, "POST", "/api/jobs", consented).await;
    let done = finished(service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{}", done["summary"]);
    // The plan role ran on Codex (read-only), the implement role on Codex in
    // a worktree, and the conversation applied the diff.
    assert_eq!(
        fs::read_to_string(setup.project.join("note.txt")).unwrap(),
        "from codex\n"
    );
    let sandboxes = lines(&setup.fake, "sandbox.log");
    assert_eq!(sandboxes.len(), 2, "{sandboxes:?}");
    assert_eq!(sandboxes[0]["sandbox"], "read-only");
    assert_eq!(sandboxes[1]["sandbox"], "workspace-write");
    let runs: Vec<Value> = events(service, &sid, "subagent.finished")
        .into_iter()
        .map(|e| e["payload"].clone())
        .filter(|r| r["status"] == "completed")
        .collect();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0]["role"], "plan");
    assert_eq!(runs[0]["runner"], "vendor");
    assert_eq!(runs[1]["role"], "implement");
    assert_eq!(events(service, &sid, "subagent.applied").len(), 1);
    // Every vendor prompt carried the subagent's instructions.
    let prompts = lines(&setup.fake, "prompts.log");
    assert!(prompts[0]
        .as_str()
        .unwrap()
        .starts_with("You are the subagent 'plan'"));
    // The parent task's usage covers its children.
    assert!(done["usage"]["total_tokens"].as_u64().unwrap() > 15 * 2);
    assert!(setup.requests.lock().unwrap().len() >= 3);
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_agent_file_can_name_a_vendor_runner() {
    let setup = setup(
        json!({"auth":"chatgpt","turn":"ok","replies":{"subagent 'auditor'":"Audit: fine"}}),
        json!({}),
        |body| {
            let message = last(body);
            if message["role"] == "tool" {
                let output = tool_output(&message);
                assert_eq!(output["output"]["model"], "Codex", "{output}");
                assert_eq!(output["output"]["ok"], true, "{output}");
                assert!(output["output"]["role"].is_null());
                return answer("Audited.", json!([]));
            }
            answer(
                "",
                json!([tool(
                    "spawn_agent",
                    json!({"agent":"auditor","prompt":"Audit it"})
                )]),
            )
        },
    )
    .await;
    let service = &setup.service;
    fs::create_dir_all(setup.project.join(".claude/agents")).unwrap();
    fs::write(
        setup.project.join(".claude/agents/auditor.md"),
        "---\nname: auditor\ndescription: Audits\nmodel: cli:codex\n---\nAudit the code.\n",
    )
    .unwrap();
    // The conversation already allowed Codex (as a role, earlier).
    let session = call(service, "POST", "/api/sessions", json!({})).await;
    let sid = session["id"].as_str().unwrap().to_owned();
    roles::record_consent(&service.engine.store(), &sid, &["cli:codex".to_owned()]).unwrap();
    let job = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"Audit the project","session_id":sid}),
    )
    .await;
    let done = finished(service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{}", done["summary"]);
    let prompts = lines(&setup.fake, "prompts.log");
    assert_eq!(prompts.len(), 1);
    let prompt = prompts[0].as_str().unwrap();
    assert!(prompt.contains("Audit the code."), "{prompt}");
    assert!(prompt.contains("<task>\nAudit it\n</task>"), "{prompt}");
    let runs = events(service, &sid, "subagent.finished");
    assert_eq!(runs[0]["payload"]["runner"], "vendor");
    assert_eq!(runs[0]["payload"]["model_id"], "cli:codex");
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn declining_the_apply_approval_leaves_the_project_unchanged() {
    let setup = setup(
        json!({
            "auth":"chatgpt",
            "turn":"ok",
            "mode_when":{"implement role":"edit"},
            "edits":{"note.txt":"new\n"},
        }),
        json!({"permissions":{"mode":"ask"}}),
        |_| answer("unused", json!([])),
    )
    .await;
    let service = &setup.service;
    call(
        service,
        "POST",
        "/api/roles",
        json!({"plan":roles::SKIP,"review":roles::SKIP,"implement":"","pipeline":true}),
    )
    .await;
    // The conversation itself runs on Codex; its roles default to it.
    let job = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"Update the note","model":"cli:codex","roles":true}),
    )
    .await;
    let sid = job["session_id"].as_str().unwrap().to_owned();
    let task_id = job["task_id"].as_str().unwrap().to_owned();
    let approval = eventually(
        || {
            service
                .engine
                .approvals()
                .list(Some(sid.as_str()))
                .into_iter()
                .find(|a| a.task_id == task_id)
        },
        "the apply approval",
    )
    .await;
    service
        .engine
        .approvals()
        .decide(&approval.id, &sid, false)
        .unwrap();
    let done = finished(service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{}", done["summary"]);
    let summary = done["summary"].as_str().unwrap();
    assert!(
        summary.contains("The changes were not applied"),
        "{summary}"
    );
    assert_eq!(
        fs::read_to_string(setup.project.join("note.txt")).unwrap(),
        "old\n"
    );
    let finished_roles = events(service, &sid, "roles.finished");
    assert_eq!(finished_roles[0]["payload"]["applied"], false);
    assert_eq!(finished_roles[0]["payload"]["completed"], false);
    assert!(events(service, &sid, "subagent.applied").is_empty());
    // A skipped review leaves only the implement role's card.
    assert_eq!(events(service, &sid, "subagent.finished").len(), 1);
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn roles_of_a_cloud_conversation_are_not_allowed_for_later_local_turns() {
    // Codex saw this conversation while it ran on Codex, so no dialog was
    // needed. That is no consent for turns that run on this computer later.
    let setup = setup(
        json!({"auth":"chatgpt","turn":"ok","replies":{"plan role":"1. Keep it small."}}),
        json!({}),
        |_| answer("Kept on this computer.", json!([])),
    )
    .await;
    let service = &setup.service;
    call(service, "POST", "/api/roles", json!({"plan":"cli:codex"})).await;
    let plan = |task: &str, sid: Option<&str>| {
        let mut body = json!({"task":task,"purpose":"planner","model":"cli:codex","roles":true});
        if let Some(sid) = sid {
            body["session_id"] = json!(sid);
        }
        body
    };
    let job = call(service, "POST", "/api/jobs", plan("Plan the change", None)).await;
    assert!(job["needs_consent"].is_null(), "{job}");
    let sid = job["session_id"].as_str().unwrap().to_owned();
    let done = finished(service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{}", done["summary"]);
    assert!(roles::consented(&service.engine.store(), &sid).is_empty());
    // Another roles turn in the cloud asks nothing: Codex saw it all.
    let again = call(
        service,
        "POST",
        "/api/jobs",
        plan("Plan the next step", Some(&sid)),
    )
    .await;
    assert!(again["needs_consent"].is_null(), "{again}");
    let done = finished(service, again["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{}", done["summary"]);
    // A private turn on this computer. Codex would receive it now, so the
    // next roles turn and an @plan request both ask first.
    call(
        service,
        "POST",
        "/api/models/register",
        json!({"id":"here","name":"fixture","provider":"local","endpoint":setup._model.endpoint,"context_limit":32768}),
    )
    .await;
    let local = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"Note a private detail","model":"here","session_id":sid}),
    )
    .await;
    assert!(local["needs_consent"].is_null(), "{local}");
    let done = finished(service, local["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{}", done["summary"]);
    let ask = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"Plan the last step","purpose":"planner","model":"here","session_id":sid,"roles":true}),
    )
    .await;
    assert_eq!(ask["needs_consent"], true, "{ask}");
    let ask = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"@plan the last step","model":"here","session_id":sid}),
    )
    .await;
    assert_eq!(ask["needs_consent"], true, "{ask}");
    assert_eq!(lines(&setup.fake, "prompts.log").len(), 2);
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_start_that_cannot_ask_runs_and_the_cloud_role_is_refused() {
    // Compare lanes, automations and the CLI start without a consent
    // dialog: an @plan on a cloud role does not stop the start; the
    // subagent is refused when it runs, and nothing reaches Codex.
    let setup = setup(json!({"auth":"chatgpt","turn":"ok"}), json!({}), |body| {
        let message = last(body);
        if message["role"] == "tool" {
            let output = tool_output(&message).to_string();
            assert!(output.contains("without your consent"), "{output}");
            return answer("Refused as expected.", json!([]));
        }
        answer("Done.", json!([]))
    })
    .await;
    let service = &setup.service;
    call(service, "POST", "/api/roles", json!({"plan":"cli:codex"})).await;
    let job = service
        .engine
        .start(shadowcode_core::engine::StartRequest {
            workspace: setup.project.clone(),
            task: "@plan the edit".into(),
            session_id: None,
            model: None,
            mode: "code".into(),
            queue: false,
            images: Vec::new(),
            web: false,
        })
        .await
        .unwrap();
    let done = finished(service, &job.id).await;
    assert_eq!(done["status"], "completed", "{}", done["summary"]);
    assert!(
        setup.fake.marker("prompts.log").is_none(),
        "Codex never ran"
    );
    assert!(roles::consented(&service.engine.store(), &job.session_id).is_empty());
    service.engine.shutdown().await.unwrap();
}

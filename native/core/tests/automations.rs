//! Scheduled automations end to end against a fake model: runs in their own
//! tagged conversations, history with status/duration/usage, no overlapping
//! runs, catch-up and missed times, fresh worktrees that are removed when
//! nothing changed, approval requests stopping unattended runs, and
//! recovery of runs interrupted by a restart.
mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
    store::{keys, AutomationRun, Store},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn response(text: &str, calls: Value) -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":if calls.as_array().is_some_and(|a|!a.is_empty()){"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30}})
}
fn tool(name: &str, args: Value) -> Value {
    json!({"id":shadowcode_core::id(),"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn git(dir: &Path, args: &[&str]) {
    let result = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

struct Fixture {
    _root: tempfile::TempDir,
    project: PathBuf,
    paths: AppPaths,
    service: Service,
}

fn setup(endpoint: &str, approve_shell: bool) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    git(&project, &["init", "-b", "main"]);
    git(&project, &["config", "user.name", "Automation Test"]);
    git(
        &project,
        &["config", "user.email", "automation@example.invalid"],
    );
    fs::write(project.join("README.md"), "# Demo\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "Initial commit"]);
    let project = project.canonicalize().unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"model":{"provider":"local","endpoint":endpoint,"name":"fixture","context_limit":16384},"trusted_workspaces":[project],"permissions":{"approve_shell":approve_shell},"agent":{"max_steps":8,"retry_attempts":0}})).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    Fixture {
        _root: root,
        project,
        paths,
        service,
    }
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

fn automation(name: &str, checkout: &str) -> Value {
    json!({
        "name": name,
        "prompt": "Look at the project and report.",
        "mode": "code",
        "schedule": {"kind": "hourly", "minute": 0},
        "timezone": "utc",
        "options": {"checkout": checkout, "max_runtime_minutes": 5, "catch_up_minutes": 60},
    })
}

async fn finished(fixture: &Fixture, id: &str) -> AutomationRun {
    tokio::time::timeout(
        Duration::from_secs(20),
        fixture.service.engine.wait_automation(id),
    )
    .await
    .expect("automation run finished")
    .unwrap()
    .expect("a history row")
}

#[tokio::test]
async fn runs_are_tagged_recorded_and_never_overlap() {
    let server = support::server(|_, _| {
        (
            response("Checked the project; all good.", json!([])),
            Duration::from_millis(600),
        )
    })
    .await;
    let fixture = setup(&server.endpoint, false);
    let service = &fixture.service;
    let created = call(
        service,
        "POST",
        "/api/automations",
        automation("Hourly check", "main"),
    )
    .await
    .unwrap();
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["description"], "Every hour at :00 UTC");
    let next = created["next_run_at"].as_f64().unwrap();
    assert!(next > shadowcode_core::now() && next <= shadowcode_core::now() + 3600.0);
    assert_eq!(created["options"]["on_approval"], "stop", "safe default");

    let run = call(
        service,
        "POST",
        &format!("/api/automations/{id}/run"),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(run["status"], "running");
    assert_eq!(run["trigger"], "manual");
    // A second "Run now" and a schedule tick during the run both refuse to
    // start another one.
    let again = call(
        service,
        "POST",
        &format!("/api/automations/{id}/run"),
        json!({}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(again.contains("already running"), "{again}");
    let rows = service.engine.automation_tick(next + 1.0).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "skipped");
    let listed = call(service, "GET", "/api/automations", Value::Null)
        .await
        .unwrap();
    assert_eq!(
        listed["automations"][0]["running_run"], run["id"],
        "the list shows the active run"
    );
    assert!(listed["automations"][0]["next_run_at"].as_f64().unwrap() > next);

    let done = finished(&fixture, &id).await;
    assert_eq!(done.id, run["id"].as_str().unwrap());
    assert_eq!(done.status, "completed", "{done:?}");
    assert!(done.summary.contains("all good"), "{}", done.summary);
    assert!(done.duration().unwrap() >= 0.5);
    assert_eq!(done.usage.as_ref().unwrap()["total_tokens"], 30);
    // The model received the automation's prompt as the task.
    assert!(server.requests.lock().unwrap().iter().any(|r| r["messages"]
        .to_string()
        .contains("Look at the project and report.")));
    // Its own conversation, tagged with the automation, in the project.
    let store = service.engine.store();
    let sid = done.session_id.clone().unwrap();
    assert_eq!(
        store.session_meta(&sid, keys::AUTOMATION_ID).unwrap(),
        Some(id.clone())
    );
    assert_eq!(
        store.session_meta(&sid, keys::AUTOMATION_RUN).unwrap(),
        Some(done.id.clone())
    );
    let session = store.session(&sid).unwrap().unwrap();
    assert_eq!(session["workspace"], json!(fixture.project));
    assert_eq!(session["title"], "Hourly check · automation");

    let detail = call(
        service,
        "GET",
        &format!("/api/automations/{id}"),
        Value::Null,
    )
    .await
    .unwrap();
    let runs = detail["runs"].as_array().unwrap();
    let mut statuses: Vec<_> = runs
        .iter()
        .map(|r| r["status"].as_str().unwrap().to_owned())
        .collect();
    statuses.sort();
    assert_eq!(statuses, ["completed", "skipped"]);
    let completed = runs.iter().find(|r| r["status"] == "completed").unwrap();
    assert!(completed["duration"].as_f64().unwrap() > 0.0);
    assert_eq!(detail["running_run"], Value::Null);

    // Pause stops the schedule; resume starts it from now; delete removes it.
    let paused = call(
        service,
        "POST",
        &format!("/api/automations/{id}/pause"),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(paused["paused"], true);
    assert_eq!(paused["next_run_at"], Value::Null);
    assert!(service
        .engine
        .automation_tick(next + 7200.0)
        .await
        .unwrap()
        .is_empty());
    let resumed = call(
        service,
        "POST",
        &format!("/api/automations/{id}/resume"),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(resumed["paused"], false);
    assert!(resumed["next_run_at"].as_f64().unwrap() > shadowcode_core::now());
    call(
        service,
        "DELETE",
        &format!("/api/automations/{id}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert!(call(service, "GET", "/api/automations", Value::Null)
        .await
        .unwrap()["automations"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn missed_times_catch_up_within_the_window_and_are_recorded_otherwise() {
    let server = support::server(|_, _| (response("Done.", json!([])), Duration::ZERO)).await;
    let fixture = setup(&server.endpoint, false);
    let service = &fixture.service;
    let store = service.engine.store();
    let recent = call(
        service,
        "POST",
        "/api/automations",
        automation("Recent", "main"),
    )
    .await
    .unwrap();
    let old = call(
        service,
        "POST",
        "/api/automations",
        automation("Old", "main"),
    )
    .await
    .unwrap();
    let paused = call(
        service,
        "POST",
        "/api/automations",
        automation("Paused", "main"),
    )
    .await
    .unwrap();
    let (recent, old, paused) = (
        recent["id"].as_str().unwrap(),
        old["id"].as_str().unwrap(),
        paused["id"].as_str().unwrap(),
    );
    call(
        service,
        "POST",
        &format!("/api/automations/{paused}/pause"),
        json!({}),
    )
    .await
    .unwrap();
    // As if ShadowCode had been closed: one time 30 minutes ago (inside the
    // 60 minute window), one five hours ago (outside it).
    let now = 1_900_000_000.0; // a fixed instant: 2030-03-17T17:46:40Z
    store
        .set_automation_next_run(recent, Some(now - 1800.0))
        .unwrap();
    store
        .set_automation_next_run(old, Some(now - 5.0 * 3600.0 - 60.0))
        .unwrap();
    let rows = service.engine.automation_tick(now).await.unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let caught = rows.iter().find(|r| r.automation_id == recent).unwrap();
    assert_eq!(caught.trigger, "catch_up");
    assert_eq!(caught.status, "running");
    let missed = rows.iter().find(|r| r.automation_id == old).unwrap();
    assert_eq!(missed.status, "missed");
    assert_eq!(missed.missed, 6, "hourly times from 5h01m ago until now");
    assert!(missed.detail.contains("catch-up window"));
    // Both move to their next time after `now`; nothing fires twice.
    for id in [recent, old] {
        let next = store.automation(id).unwrap().next_run_at.unwrap();
        assert!(next > now && next <= now + 3600.0);
    }
    assert!(service
        .engine
        .automation_tick(now)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(finished(&fixture, recent).await.status, "completed");
    assert!(store.automation_runs(paused, 10).unwrap().is_empty());
}

#[tokio::test]
async fn worktree_runs_remove_an_untouched_checkout_and_keep_one_with_changes() {
    let server = support::server(|_, body| {
        let messages = body["messages"].as_array().unwrap();
        let answered = messages.iter().any(|m| m["role"] == "tool");
        let value = if body["messages"].to_string().contains("Write the report") && !answered {
            response(
                "Writing",
                json!([tool(
                    "write_file",
                    json!({"path":"report.txt","content":"ok\n","expected_hash":"missing"})
                )]),
            )
        } else {
            response("Finished.", json!([]))
        };
        (value, Duration::ZERO)
    })
    .await;
    let fixture = setup(&server.endpoint, false);
    let service = &fixture.service;
    let store = service.engine.store();

    let quiet = call(
        service,
        "POST",
        "/api/automations",
        automation("Quiet", "worktree"),
    )
    .await
    .unwrap();
    let quiet = quiet["id"].as_str().unwrap();
    call(
        service,
        "POST",
        &format!("/api/automations/{quiet}/run"),
        json!({}),
    )
    .await
    .unwrap();
    let run = finished(&fixture, quiet).await;
    assert_eq!(run.status, "completed", "{run:?}");
    let tree = run.worktree.clone().unwrap();
    assert_eq!(tree["removed"], true, "{run:?}");
    assert!(!Path::new(tree["path"].as_str().unwrap()).exists());
    assert!(run.detail.contains("temporary worktree was removed"));
    // Its conversation now points at the project, and the checkout is no
    // longer trusted.
    let session = store
        .session(run.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(session["workspace"], json!(fixture.project));
    let cfg = Config::load(&fixture.paths, None).unwrap();
    assert!(!cfg.is_trusted(Path::new(tree["path"].as_str().unwrap())));

    let mut busy = automation("Writer", "worktree");
    busy["prompt"] = json!("Write the report file.");
    let busy = call(service, "POST", "/api/automations", busy)
        .await
        .unwrap();
    let busy = busy["id"].as_str().unwrap();
    call(
        service,
        "POST",
        &format!("/api/automations/{busy}/run"),
        json!({}),
    )
    .await
    .unwrap();
    let run = finished(&fixture, busy).await;
    assert_eq!(run.status, "completed", "{run:?}");
    let tree = run.worktree.clone().unwrap();
    assert_ne!(tree["removed"], true);
    let checkout = PathBuf::from(tree["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(checkout.join("report.txt")).unwrap(),
        "ok\n"
    );
    assert!(
        !fixture.project.join("report.txt").exists(),
        "the project itself is untouched"
    );
    assert!(
        run.detail.contains(tree["branch"].as_str().unwrap()),
        "{}",
        run.detail
    );
    // Opening that conversation never makes the worktree a project.
    let sid = run.session_id.clone().unwrap();
    call(
        service,
        "POST",
        &format!("/api/sessions/{sid}/activate"),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(service.workspace().unwrap(), checkout);
    let projects = call(service, "GET", "/api/projects", Value::Null)
        .await
        .unwrap();
    assert!(
        !projects.to_string().contains(checkout.to_str().unwrap()),
        "{projects}"
    );
}

/// A follow-up queued in a worktree run's conversation keeps the worktree:
/// cleanup never removes a checkout another task is about to use.
#[tokio::test]
async fn worktree_cleanup_waits_for_a_queued_follow_up() {
    let server = support::server(|_, body| {
        let text = body["messages"].to_string();
        let answered = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool");
        if text.contains("Follow up") && !answered {
            return (
                response(
                    "Writing",
                    json!([tool(
                        "write_file",
                        json!({"path":"follow.txt","content":"kept\n","expected_hash":"missing"})
                    )]),
                ),
                // Still working when the automation's cleanup runs.
                Duration::from_millis(3000),
            );
        }
        let slow = !text.contains("Follow up");
        (
            response("Finished.", json!([])),
            if slow {
                Duration::from_millis(1500)
            } else {
                Duration::ZERO
            },
        )
    })
    .await;
    let fixture = setup(&server.endpoint, false);
    let service = &fixture.service;
    let store = service.engine.store();
    let created = call(
        service,
        "POST",
        "/api/automations",
        automation("Slow", "worktree"),
    )
    .await
    .unwrap();
    let id = created["id"].as_str().unwrap().to_owned();
    call(
        service,
        "POST",
        &format!("/api/automations/{id}/run"),
        json!({}),
    )
    .await
    .unwrap();
    // The run's conversation and worktree, while its job is still running.
    let (sid, checkout) = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(run) = store.automation_runs(&id, 1).unwrap().into_iter().next() {
                if let (Some(sid), Some(tree), Some(_)) =
                    (run.session_id.clone(), run.worktree.clone(), run.job_id)
                {
                    return (sid, PathBuf::from(tree["path"].as_str().unwrap()));
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let follow = service
        .engine
        .start(shadowcode_core::engine::StartRequest {
            workspace: checkout.clone(),
            task: "Follow up: write the note.".into(),
            session_id: Some(sid.clone()),
            model: None,
            mode: "code".into(),
            queue: true,
            images: Vec::new(),
            web: false,
        })
        .await
        .unwrap();
    let run = finished(&fixture, &id).await;
    assert_eq!(run.status, "completed", "{run:?}");
    let follow = tokio::time::timeout(Duration::from_secs(20), service.engine.wait(&follow.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(follow.status, "completed", "{}", follow.summary);
    let tree = run.worktree.clone().unwrap();
    assert_ne!(tree["removed"], true, "{run:?}");
    // Kept because the follow-up used it (or, on a very slow machine, had
    // already written its file there by the time of the cleanup).
    assert!(
        run.detail.contains("was kept") || run.detail.contains(tree["branch"].as_str().unwrap()),
        "{}",
        run.detail
    );
    assert_eq!(
        fs::read_to_string(checkout.join("follow.txt")).unwrap(),
        "kept\n"
    );
    let session = store.session(&sid).unwrap().unwrap();
    assert_eq!(session["workspace"], json!(checkout));
}

#[tokio::test]
async fn approval_requests_stop_an_unattended_run() {
    let server = support::server(|_, _| {
        (
            response(
                "Running tests",
                json!([tool("exec", json!({"command":"echo hi"}))]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let fixture = setup(&server.endpoint, true);
    let service = &fixture.service;
    let created = call(
        service,
        "POST",
        "/api/automations",
        automation("Tests", "main"),
    )
    .await
    .unwrap();
    let id = created["id"].as_str().unwrap();
    call(
        service,
        "POST",
        &format!("/api/automations/{id}/run"),
        json!({}),
    )
    .await
    .unwrap();
    let run = finished(&fixture, id).await;
    assert_eq!(run.status, "needs_approval", "{run:?}");
    assert!(run.detail.contains("echo hi"), "{}", run.detail);
    let job = service
        .engine
        .job(run.job_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(job.status, "cancelled");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stuck_unattended_run_goes_on_instead_of_pausing() {
    let server = support::server(|index, _| {
        if index < 3 {
            (
                response(
                    "",
                    json!([tool(
                        "exec",
                        json!({"command":"sh -c 'echo \"test failed: expected 3\"; exit 3'"})
                    )]),
                ),
                Duration::ZERO,
            )
        } else {
            (
                response("The test fails the same way each time.", json!([])),
                Duration::ZERO,
            )
        }
    })
    .await;
    let fixture = setup(&server.endpoint, false);
    let service = &fixture.service;
    let created = call(
        service,
        "POST",
        "/api/automations",
        automation("Tests", "main"),
    )
    .await
    .unwrap();
    let id = created["id"].as_str().unwrap();
    call(
        service,
        "POST",
        &format!("/api/automations/{id}/run"),
        json!({}),
    )
    .await
    .unwrap();
    // Nobody could answer a stuck card: the run is not parked until its
    // time limit, and the agent is told to change course.
    let run = finished(&fixture, id).await;
    assert_eq!(run.status, "completed", "{run:?}");
    let events = service
        .engine
        .store()
        .recent_events(run.session_id.as_deref().unwrap(), 300)
        .unwrap();
    let stuck: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "agent.stuck")
        .collect();
    assert_eq!(stuck.len(), 1);
    assert_eq!(stuck[0]["payload"]["paused"], false);
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);
    assert!(requests[3]["messages"]
        .to_string()
        .contains("Stuck check: The agent seems stuck"));
}

#[tokio::test]
async fn invalid_automations_and_untrusted_projects_are_refused() {
    let server = support::server(|_, _| (response("x", json!([])), Duration::ZERO)).await;
    let fixture = setup(&server.endpoint, false);
    let service = &fixture.service;
    for (patch, expected) in [
        (json!({"name": ""}), "name"),
        (json!({"mode": "yolo"}), "Mode"),
        (
            json!({"schedule": {"kind": "cron", "expr": "61 * * * *"}}),
            "minute",
        ),
        (
            json!({"schedule": {"kind": "cron", "expr": "0 0 30 2 *"}}),
            "never runs",
        ),
        (json!({"options": {"max_runtime_minutes": 0}}), "time limit"),
    ] {
        let mut body = automation("Bad", "main");
        for (key, value) in patch.as_object().unwrap() {
            body[key] = value.clone();
        }
        let error = call(service, "POST", "/api/automations", body)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains(expected),
            "{expected}: {error:#}"
        );
    }
    let preview = call(
        service,
        "POST",
        "/api/automations/preview",
        json!({"schedule": {"kind": "weekdays", "time": "09:00"}, "timezone": "utc"}),
    )
    .await
    .unwrap();
    assert_eq!(preview["ok"], true);
    assert_eq!(preview["description"], "Weekdays at 09:00 UTC");
    assert_eq!(preview["next"].as_array().unwrap().len(), 3);
    let bad = call(
        service,
        "POST",
        "/api/automations/preview",
        json!({"schedule": {"kind": "cron", "expr": "nope"}}),
    )
    .await
    .unwrap();
    assert_eq!(bad["ok"], false);

    Config::update(&fixture.paths, |cfg| {
        cfg.trusted_workspaces.clear();
        Ok(())
    })
    .unwrap();
    let error = call(
        service,
        "POST",
        "/api/automations",
        automation("Untrusted", "main"),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("Trust"), "{error}");
}

#[test]
fn runs_interrupted_by_a_restart_are_recovered() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("state.sqlite");
    let store = Store::open(&db).unwrap();
    let run = AutomationRun {
        id: "r1".into(),
        automation_id: "a1".into(),
        status: "running".into(),
        trigger: "schedule".into(),
        started_at: 10.0,
        ..Default::default()
    };
    store.save_automation_run(&run).unwrap();
    drop(store);
    let store = Store::open(&db).unwrap();
    assert_eq!(store.recover_automation_runs().unwrap(), 1);
    let recovered = store.automation_run("r1").unwrap();
    assert_eq!(recovered.status, "interrupted");
    assert!(recovered.finished_at.is_some());
    assert!(recovered
        .detail
        .contains("stopped before this run finished"));
    assert_eq!(store.recover_automation_runs().unwrap(), 0);
}

/// A run on a paid model that reaches its spending limit stops as
/// `spending_limit` (the automation answers the card; it is not a failure).
#[tokio::test(flavor = "multi_thread")]
async fn a_paid_run_at_its_spending_limit_stops_as_spending_limit() {
    let server = support::server(|index, _| {
        let calls = if index < 3 {
            json!([tool("list_files", json!({"path":"."}))])
        } else {
            json!([])
        };
        let reason = if index < 3 { "tool_calls" } else { "stop" };
        (
            json!({"choices":[{"message":{"role":"assistant","content":"Looking.","tool_calls":calls},"finish_reason":reason}],
                "usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"cost":0.08}}),
            Duration::ZERO,
        )
    })
    .await;
    let fixture = setup(&server.endpoint, false);
    Config::patch(
        &fixture.paths,
        json!({
            "model":{"provider":"openrouter","endpoint":server.endpoint,"name":"acme/coder","api_key_env":"SHADOWCODE_TEST_UNUSED_API_KEY","context_limit":32768},
            "spending":{"task_usd":0.1,"daily_usd":null},
        }),
    )
    .unwrap();
    let service = &fixture.service;
    let created = call(
        service,
        "POST",
        "/api/automations",
        automation("Paid", "main"),
    )
    .await
    .unwrap();
    let id = created["id"].as_str().unwrap();
    call(
        service,
        "POST",
        &format!("/api/automations/{id}/run"),
        json!({}),
    )
    .await
    .unwrap();
    let run = finished(&fixture, id).await;
    assert_eq!(run.status, "spending_limit", "{run:?}");
    assert!(
        run.detail
            .starts_with("Stopped at the spending limit for paid models"),
        "{}",
        run.detail
    );
    // The limit stopped the run before another paid request.
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

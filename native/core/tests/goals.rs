mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    engine::Engine,
    paths::AppPaths,
    service::{Request, Service},
    store::{MilestoneSpec, Store},
};
use std::{fs, time::Duration};

fn response(text: &str, calls: Value) -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":if calls.as_array().is_some_and(|a|!a.is_empty()){"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30}})
}
fn tool(name: &str, args: Value) -> Value {
    json!({"id":shadowcode_core::id(),"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn setup(endpoint: &str) -> (tempfile::TempDir, Service) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths,json!({"model":{"provider":"local","endpoint":endpoint,"name":"fixture","context_limit":16384},"trusted_workspaces":[project],"permissions":{"approve_shell":false},"agent":{"max_steps":8,"retry_attempts":0}})).unwrap();
    (root, Service::open(paths, Some(project)).unwrap())
}
fn plan(titles: &[&str], verify: bool) -> Vec<MilestoneSpec> {
    titles
        .iter()
        .enumerate()
        .map(|(index, title)| MilestoneSpec {
            title: (*title).into(),
            mode: "code".into(),
            require_verification: verify && index == titles.len() - 1,
        })
        .collect()
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
async fn done(engine: &Engine, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(8), engine.wait_goal(id))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn milestones_execute_tools_verify_persist_and_never_rerun_completed_work() {
    let server = support::server(|index, body| {
        shadowcode_core::context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let value = match index {
            0 => response(
                "Writing",
                json!([tool(
                    "write_file",
                    json!({"path":"proof.txt","content":"verified\n","expected_hash":"missing"})
                )]),
            ),
            1 => response("The file was created.", json!([])),
            2 => response(
                "Checking",
                json!([tool(
                    "exec",
                    json!({"command":"test \"$(cat proof.txt)\" = verified"})
                )]),
            ),
            _ => response("The acceptance command passed.", json!([])),
        };
        (value, Duration::ZERO)
    })
    .await;
    let (root, service) = setup(&server.endpoint);
    Config::patch(
        service.engine.paths(),
        json!({"verification":{"commands":["test \"$(cat proof.txt)\" = verified"]}}),
    )
    .unwrap();
    let goal=call(&service,"POST","/api/goals",json!({"instruction":"Write proof.txt and check its contents.","milestones":plan(&["Build file","Run check"],true),"run":true})).await.unwrap();
    let id = goal["id"].as_str().unwrap();
    let result = done(&service.engine, id).await;
    assert_eq!(result["status"], "completed", "{result}");
    assert_eq!(result["progress_pct"], 100);
    assert_eq!(result["running"], false);
    assert!(result["milestones"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["status"] == "done" && !m["task_id"].as_str().unwrap().is_empty()));
    assert_eq!(
        fs::read_to_string(root.path().join("project/proof.txt")).unwrap(),
        "verified\n"
    );
    let jobs = service.engine.store().jobs(10).unwrap();
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().all(|j| j["session_id"] == result["session_id"]));
    assert!(
        call(&service, "POST", &format!("/api/goals/{id}/run"), json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("already complete")
    );
    assert_eq!(server.requests.lock().unwrap().len(), 4);
    service.engine.shutdown().await.unwrap();
    drop(service);
    let reopened = Service::open(
        AppPaths::isolated(&root.path().join("profile")).unwrap(),
        Some(root.path().join("project")),
    )
    .unwrap();
    assert_eq!(
        reopened.engine.store().goal(id).unwrap()["status"],
        "completed"
    );
    reopened.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn pause_cancels_the_active_job_and_resume_skips_completed_milestones() {
    let server = support::server(|index, _| {
        (
            response("Milestone answered", json!([])),
            if index == 0 {
                Duration::ZERO
            } else {
                Duration::from_secs(5)
            },
        )
    })
    .await;
    let (_root, service) = setup(&server.endpoint);
    let goal=call(&service,"POST","/api/goals",json!({"instruction":"Explain three concepts","milestones":plan(&["First","Second","Third"],false),"run":true})).await.unwrap();
    let id = goal["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while server.requests.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for (method, path, body) in [
        ("POST", format!("/api/goals/{id}/run"), json!({})),
        ("DELETE", format!("/api/goals/{id}"), json!({})),
        (
            "POST",
            format!(
                "/api/goals/{id}/milestones/{}",
                goal["milestones"][0]["id"].as_str().unwrap()
            ),
            json!({"status":"done"}),
        ),
        (
            "DELETE",
            format!("/api/sessions/{}", goal["session_id"].as_str().unwrap()),
            json!({}),
        ),
    ] {
        assert!(call(&service, method, &path, body).await.is_err());
    }
    let paused = tokio::time::timeout(
        Duration::from_secs(2),
        call(
            &service,
            "POST",
            &format!("/api/goals/{id}/pause"),
            json!({}),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(paused["status"], "paused");
    assert_eq!(paused["milestones"][0]["status"], "done");
    assert_eq!(paused["milestones"][1]["status"], "pending");
    assert_eq!(paused["running"], false);
    assert_eq!(
        service.engine.store().jobs(10).unwrap()[0]["status"],
        "cancelled"
    );
    let resumed = support::server(|_, _| {
        (
            response("Remaining concept answered", json!([])),
            Duration::ZERO,
        )
    })
    .await;
    Config::patch(
        service.engine.paths(),
        json!({"model":{"endpoint":resumed.endpoint}}),
    )
    .unwrap();
    call(&service, "POST", &format!("/api/goals/{id}/run"), json!({}))
        .await
        .unwrap();
    assert_eq!(done(&service.engine, id).await["status"], "completed");
    assert_eq!(resumed.requests.lock().unwrap().len(), 2);
    assert_eq!(
        service.engine.store().goal(id).unwrap()["milestones"][0]["task_id"],
        paused["milestones"][0]["task_id"]
    );
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_default_inspection_milestone_requires_a_current_workspace_read() {
    let server = support::server(|_, _| {
        (
            response("I inspected everything; ready to implement.", json!([])),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, service) = setup(&server.endpoint);
    let goal = call(
        &service,
        "POST",
        "/api/goals",
        json!({"instruction":"Build a small tool","run":true}),
    )
    .await
    .unwrap();
    let result = done(&service.engine, goal["id"].as_str().unwrap()).await;
    assert_eq!(result["status"], "blocked");
    assert_eq!(result["milestones"][0]["status"], "failed");
    assert_eq!(result["milestones"][1]["status"], "pending");
    assert!(result["run_detail"]
        .as_str()
        .unwrap()
        .contains("did not inspect"));
    assert_eq!(service.engine.store().jobs(10).unwrap().len(), 1);
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn verification_requires_a_real_successful_command_and_a_failed_check_stops_progress() {
    for (command, verify) in [
        (None, true),
        (Some("exit 7"), true),
        (Some("exit 7"), false),
        (Some("printf test"), true),
        (Some("pwd"), true),
    ] {
        let server = support::server(move |index, _| {
            let value = if let (0, Some(command)) = (index, command) {
                response(
                    "Checking",
                    json!([tool("exec", json!({"command":command}))]),
                )
            } else {
                response("Everything is done and verified!", json!([]))
            };
            (value, Duration::ZERO)
        })
        .await;
        let (_root, service) = setup(&server.endpoint);
        let goal=call(&service,"POST","/api/goals",json!({"instruction":"Complete a verification milestone","milestones":plan(&["Acceptance"],verify),"run":true})).await.unwrap();
        let result = done(&service.engine, goal["id"].as_str().unwrap()).await;
        assert_eq!(result["status"], "blocked", "{result}");
        assert_eq!(result["progress_pct"], 0);
        assert_eq!(result["milestones"][0]["status"], "failed");
        assert!(result["run_detail"]
            .as_str()
            .unwrap()
            .contains("verification is"));
        service.engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn goal_edits_are_scoped_and_workspace_session_mismatches_do_not_start_work() {
    let (root, service) = setup("http://127.0.0.1:9/v1");
    let a = call(
        &service,
        "POST",
        "/api/goals",
        json!({"instruction":"First"}),
    )
    .await
    .unwrap();
    let b = call(
        &service,
        "POST",
        "/api/goals",
        json!({"instruction":"Second"}),
    )
    .await
    .unwrap();
    let wrong = call(
        &service,
        "POST",
        &format!(
            "/api/goals/{}/milestones/{}",
            a["id"].as_str().unwrap(),
            b["milestones"][0]["id"].as_str().unwrap()
        ),
        json!({"status":"done"}),
    )
    .await;
    assert!(wrong
        .unwrap_err()
        .to_string()
        .contains("not found in this goal"));
    assert_eq!(
        service
            .engine
            .store()
            .goal(b["id"].as_str().unwrap())
            .unwrap()["progress_pct"],
        0
    );
    fs::create_dir(root.path().join("other")).unwrap();
    let session = service
        .engine
        .store()
        .create_session(&root.path().join("other"), "fixture", "")
        .unwrap();
    assert!(call(
        &service,
        "POST",
        &format!("/api/goals/{}/run", a["id"].as_str().unwrap()),
        json!({"session_id":session["id"]})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("different workspace"));
    assert!(service.engine.store().jobs(10).unwrap().is_empty());
    call(
        &service,
        "POST",
        &format!("/api/goals/{}/abandon", a["id"].as_str().unwrap()),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(
        service
            .engine
            .store()
            .goal(a["id"].as_str().unwrap())
            .unwrap()["status"],
        "abandoned"
    );
    call(
        &service,
        "DELETE",
        &format!("/api/goals/{}", a["id"].as_str().unwrap()),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(
        call(&service, "GET", "/api/goals", json!({}))
            .await
            .unwrap()["goals"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn resuming_a_goal_with_a_deleted_conversation_creates_a_new_session() {
    let server = support::server(|_, _| (response("Answered", json!([])), Duration::ZERO)).await;
    let (_root, service) = setup(&server.endpoint);
    let goal = call(
        &service,
        "POST",
        "/api/goals",
        json!({"instruction":"Explain a concept","milestones":plan(&["Explain"],false),"run":true}),
    )
    .await
    .unwrap();
    let id = goal["id"].as_str().unwrap();
    let completed = done(&service.engine, id).await;
    let old = completed["session_id"].as_str().unwrap();
    service.engine.delete_session(old).unwrap();
    service
        .engine
        .update_goal_milestone(
            id,
            completed["milestones"][0]["id"].as_str().unwrap(),
            "pending",
            "",
        )
        .unwrap();
    let restarted = call(&service, "POST", &format!("/api/goals/{id}/run"), json!({}))
        .await
        .unwrap();
    assert_ne!(restarted["session_id"], old);
    assert_eq!(done(&service.engine, id).await["status"], "completed");
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_waits_for_goal_cancellation_and_rejects_new_goal_runs() {
    let server =
        support::server(|_, _| (response("late", json!([])), Duration::from_secs(10))).await;
    let (_root, service) = setup(&server.endpoint);
    let goal=call(&service,"POST","/api/goals",json!({"instruction":"Explain a concept","milestones":plan(&["First","Second"],false),"run":true})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while server.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), service.engine.shutdown())
        .await
        .unwrap()
        .unwrap();
    let id = goal["id"].as_str().unwrap();
    let result = service.engine.store().goal(id).unwrap();
    assert_eq!(result["status"], "paused");
    assert_eq!(result["running"], false);
    assert!(service
        .engine
        .start_goal(id, None)
        .unwrap_err()
        .to_string()
        .contains("shutting down"));
    assert!(service
        .engine
        .store()
        .jobs(10)
        .unwrap()
        .iter()
        .all(|j| j["status"] == "cancelled"));
}

#[test]
fn restart_preserves_completed_milestones_and_never_replays_interrupted_work() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let store = Store::open(&paths.database()).unwrap();
    let goal = store
        .create_goal(
            &project,
            "Explain concepts",
            &plan(&["First", "Second"], false),
        )
        .unwrap();
    let id = goal["id"].as_str().unwrap();
    let db = rusqlite::Connection::open(&store.path).unwrap();
    db.execute(
        "UPDATE milestones SET status='done' WHERE goal_id=? AND order_index=0",
        [id],
    )
    .unwrap();
    db.execute("UPDATE milestones SET status='in_progress',task_id='interrupted-task' WHERE goal_id=? AND order_index=1",[id]).unwrap();
    db.execute("UPDATE goals SET progress=0.5 WHERE id=?", [id])
        .unwrap();
    db.execute(
        "INSERT INTO goal_runs VALUES(?,'session','interrupted-job','running','',1)",
        [id],
    )
    .unwrap();
    drop(db);
    drop(store);
    let engine = Engine::open(paths).unwrap();
    let recovered = engine.store().goal(id).unwrap();
    assert_eq!(recovered["status"], "paused");
    assert_eq!(recovered["running"], false);
    assert_eq!(recovered["progress_pct"], 50);
    assert_eq!(recovered["milestones"][0]["status"], "done");
    assert_eq!(recovered["milestones"][1]["status"], "pending");
    assert_eq!(recovered["milestones"][1]["task_id"], "interrupted-task");
    assert!(engine.store().jobs(10).unwrap().is_empty());
}

mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    approvals::Approval,
    background::BackgroundTask,
    config::{Config, PermissionLevel},
    context,
    engine::StartRequest,
    events::TaskEvents,
    models::ToolCall,
    paths::AppPaths,
    service::{Request, Service},
    tools::{ToolExecutor, ToolResult},
    workspace::Workspace,
};
use std::{fs, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn fixture(endpoint: &str) -> (tempfile::TempDir, Service) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"model":{"provider":"local","name":"fixture","endpoint":endpoint,"context_limit":16384},"trusted_workspaces":[project],"agent":{"max_steps":12,"model_retries":0}})).unwrap();
    (root, Service::open(paths, Some(project)).unwrap())
}
fn tools(service: &Service) -> ToolExecutor {
    let workspace = Arc::new(Workspace::open(&service.workspace().unwrap()).unwrap());
    let store = service.engine.store();
    let session_id = store
        .create_session(&workspace.path, "fixture", "")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let task_id = store
        .create_task(&session_id, "background tool test")
        .unwrap();
    let (sender, _) = tokio::sync::broadcast::channel(100);
    ToolExecutor::new(
        workspace,
        Config::load(service.engine.paths(), None).unwrap(),
        service.engine.approvals(),
        TaskEvents {
            store,
            session_id,
            task_id,
            sender,
        },
        CancellationToken::new(),
    )
    .unwrap()
    .with_background(service.engine.background().clone())
}
async fn call(tools: &ToolExecutor, name: &str, arguments: Value) -> ToolResult {
    tools
        .execute(ToolCall {
            id: shadowcode_core::id(),
            name: name.into(),
            arguments,
        })
        .await
        .unwrap()
}
async fn pending(service: &Service) -> Approval {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(record) = service.engine.approvals().list(None).pop() {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("approval was not offered")
}
async fn until(
    service: &Service,
    id: &str,
    check: impl Fn(&BackgroundTask) -> bool,
) -> BackgroundTask {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let task = service.engine.background().get(id).unwrap();
            if check(&task) {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background process did not reach expected state")
}
fn dead(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(") ")
            .is_some_and(|(_, state)| state.starts_with('Z') || state.starts_with('X')),
        Err(_) => true,
    }
}

#[tokio::test]
async fn scoped_approvals_gate_start_stop_and_cleanup_with_durable_origin() {
    let (_root, service) = fixture("http://127.0.0.1:1/v1");
    let tools = tools(&service);
    let command = "sleep 60 & echo $! > child.pid; printf READY; wait";
    for approve in [false, true] {
        let worker = tools.clone();
        let start = tokio::spawn(async move {
            call(
                &worker,
                "background_start",
                json!({"name":"dev server","command":command}),
            )
            .await
        });
        let approval = pending(&service).await;
        assert_eq!(approval.tool, "background_start");
        assert_eq!(approval.task_id, tools.events.task_id);
        assert!(approval.command.contains(command));
        assert!(approval
            .command
            .contains(tools.workspace.path.to_str().unwrap()));
        assert!(approval.reason.contains("including cancellation"));
        assert!(approval
            .reason
            .contains("outside ShadowCode's command sandbox"));
        assert!(approval.reason.contains("file and network access"));
        assert!(!tools.workspace.path.join("child.pid").exists());
        assert!(service
            .engine
            .background()
            .list(&tools.workspace.path)
            .unwrap()
            .is_empty());
        assert!(tools
            .approvals
            .decide(&approval.id, "wrong-session", true)
            .is_err());
        tools
            .approvals
            .decide(&approval.id, &approval.session_id, approve)
            .unwrap();
        let result = start.await.unwrap();
        assert_eq!(result.success, approve, "{}", result.error);
        if !approve {
            continue;
        }
        assert_eq!(result.output["lifetime"], "project");
        let id = result.output["id"].as_str().unwrap().to_owned();
        let live = until(&service, &id, |task| task.output.contains("READY")).await;
        assert_eq!(
            live.origin_task_id.as_deref(),
            Some(tools.events.task_id.as_str())
        );
        let child: u32 = fs::read_to_string(tools.workspace.path.join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!dead(child));
        for stop_allowed in [false, true] {
            let worker = tools.clone();
            let target = id.clone();
            let stop = tokio::spawn(async move {
                call(&worker, "background_stop", json!({"id":target})).await
            });
            let approval = pending(&service).await;
            assert_eq!(approval.tool, "background_stop");
            assert!(approval.command.contains(&id) && approval.command.contains(command));
            assert!(!dead(child));
            tools
                .approvals
                .decide(&approval.id, &approval.session_id, stop_allowed)
                .unwrap();
            let result = stop.await.unwrap();
            assert_eq!(result.success, stop_allowed, "{}", result.error);
            if stop_allowed {
                assert_eq!(result.output["status"], "CANCELLED");
                assert!(dead(child) && dead(live.pid));
            } else {
                assert_eq!(
                    service.engine.background().get(&id).unwrap().status,
                    "RUNNING"
                );
            }
        }
        let events = tools
            .events
            .store
            .recent_events(&tools.events.session_id, 100)
            .unwrap();
        for kind in ["background.started", "background.completed"] {
            assert!(events
                .iter()
                .any(|e| e["type"] == kind && e["task_id"] == tools.events.task_id));
        }
    }
    // Cancelling an unanswered start never launches a process.
    let worker = tools.clone();
    let start = tokio::spawn(async move {
        call(
            &worker,
            "background_start",
            json!({"name":"cancelled","command":"touch unexpected"}),
        )
        .await
    });
    pending(&service).await;
    tools.cancel.cancel();
    assert!(!start.await.unwrap().success);
    assert!(tools.approvals.list(None).is_empty());
    assert!(!tools.workspace.path.join("unexpected").exists());
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn permissions_arguments_and_project_boundaries_are_enforced_before_control() {
    let (root, service) = fixture("http://127.0.0.1:1/v1");
    let mut tools = tools(&service);
    tools.config.permissions.approve_shell = false;
    tools.config.permissions.network = false;
    for command in ["sudo true", "npm run dev"] {
        assert!(
            !call(
                &tools,
                "background_start",
                json!({"name":"blocked","command":command})
            )
            .await
            .success
        );
    }
    tools.config.trusted_workspaces.clear();
    assert!(
        !call(
            &tools,
            "background_start",
            json!({"name":"blocked","command":"touch unexpected"})
        )
        .await
        .success
    );
    tools.config = Config::load(service.engine.paths(), None).unwrap();
    tools.config.permissions.approve_shell = false;
    let other = root.path().join("other");
    fs::create_dir(&other).unwrap();
    let foreign = BackgroundTask {
        id: "foreign-record".into(),
        name: "private".into(),
        cwd: other.to_string_lossy().into(),
        command: "foreign-command-secret".into(),
        output: "foreign-output-secret".into(),
        status: "COMPLETED".into(),
        ..Default::default()
    };
    service.engine.store().save_background(&foreign).unwrap();
    for name in ["background_output", "background_stop"] {
        let result = call(&tools, name, json!({"id":foreign.id})).await;
        assert!(!result.success && result.error.contains("another project"));
        assert!(!result.error.contains("secret"));
        assert_eq!(result.output, Value::Null);
    }
    assert!(tools.approvals.list(None).is_empty());
    let started = call(
        &tools,
        "background_start",
        json!({"name":"own","command":"printf own-output; sleep 60"}),
    )
    .await;
    assert!(started.success, "{}", started.error);
    let id = started.output["id"].as_str().unwrap();
    until(&service, id, |task| task.output.contains("own-output")).await;
    for (name, args) in [
        (
            "background_start",
            json!({"name":"bad","command":"touch unexpected","cwd":other}),
        ),
        (
            "background_start",
            json!({"name":"bad","command":"true\u{0000}"}),
        ),
        ("background_start", json!({"name":" ","command":"true"})),
        ("background_list", json!({"workspace":other})),
        ("background_output", json!({"id":id,"max_bytes":0})),
        ("background_output", json!({"id":id,"max_bytes":64001})),
        ("background_stop", json!({"id":id,"pid":123})),
    ] {
        assert!(!call(&tools, name, args).await.success, "{name}");
    }
    tools.config.permissions.level = PermissionLevel::ReadOnly;
    assert!(
        !call(
            &tools,
            "background_start",
            json!({"name":"blocked","command":"touch unexpected"})
        )
        .await
        .success
    );
    assert!(
        !call(&tools, "background_stop", json!({"id":id}))
            .await
            .success
    );
    let list = call(&tools, "background_list", json!({})).await;
    assert!(list.success && !list.output.to_string().contains("foreign"));
    let output = call(&tools, "background_output", json!({"id":id})).await;
    assert!(output.success);
    assert_eq!(output.output["output"], "own-output");
    assert!(!tools.workspace.path.join("unexpected").exists());
    assert!(tools.approvals.list(None).is_empty());
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn bounded_history_keeps_old_active_processes_and_survives_origin_deletion() {
    let (_root, service) = fixture("http://127.0.0.1:1/v1");
    let mut tools = tools(&service);
    tools.config.permissions.approve_shell = false;
    let started = call(
        &tools,
        "background_start",
        json!({"name":"watcher","command":"printf alive; sleep 60"}),
    )
    .await;
    assert!(started.success, "{}", started.error);
    let id = started.output["id"].as_str().unwrap();
    let live = until(&service, id, |task| task.output == "alive").await;
    for index in 0..110 {
        service
            .engine
            .store()
            .save_background(&BackgroundTask {
                id: format!("history-{index}"),
                cwd: live.cwd.clone(),
                status: "COMPLETED".into(),
                started_at: live.started_at + 1.0 + index as f64,
                command: "é".repeat(2000),
                output: "😀".repeat(5000),
                ..Default::default()
            })
            .unwrap();
    }
    let list = call(&tools, "background_list", json!({})).await;
    assert!(list.success);
    assert_eq!(list.output["history_truncated"], true);
    let entries = list.output["tasks"].as_array().unwrap();
    assert_eq!(entries.len(), 13);
    assert!(entries.iter().any(|entry| entry["id"] == id));
    assert!(entries
        .iter()
        .all(|entry| entry["output"].as_str().unwrap().len() <= 512
            && entry["command"].as_str().unwrap().len() <= 1000));
    let output = call(
        &tools,
        "background_output",
        json!({"id":"history-109","max_bytes":7}),
    )
    .await;
    assert!(output.success);
    assert_eq!(output.output["output"], "😀");
    assert_eq!(output.output["output_preview_truncated"], true);
    assert_eq!(output.output["command_truncated"], true);
    assert!(service
        .engine
        .delete_session(&tools.events.session_id)
        .unwrap());
    let stopped = service.engine.background().stop(id).await.unwrap();
    assert_eq!(stopped.status, "CANCELLED");
    assert_eq!(
        stopped.origin_task_id.as_deref(),
        Some(tools.events.task_id.as_str())
    );
    assert!(stopped.error.is_empty(), "{}", stopped.error);
    assert!(dead(live.pid));
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn before_command_hook_blocks_start_and_start_invalidates_prior_file_observations() {
    let (_root, service) = fixture("http://127.0.0.1:1/v1");
    Config::patch(
        service.engine.paths(),
        json!({"permissions":{"approve_shell":false}}),
    )
    .unwrap();
    let workspace = Workspace::open(&service.workspace().unwrap()).unwrap();
    workspace.write("existing.txt", b"original", None).unwrap();
    let first = tools(&service);
    assert!(
        call(&first, "read_file", json!({"path":"existing.txt"}))
            .await
            .success
    );
    let started = call(
        &first,
        "background_start",
        json!({"name":"formatter","command":"printf ready"}),
    )
    .await;
    assert!(started.success, "{}", started.error);
    until(&service, started.output["id"].as_str().unwrap(), |t| {
        t.status == "COMPLETED"
    })
    .await;
    assert!(
        !call(
            &first,
            "write_file",
            json!({"path":"existing.txt","content":"blind overwrite"})
        )
        .await
        .success
    );
    let path = ".shadowcode/hooks/background-gate.yaml";
    workspace
        .write(
            path,
            b"name: background-gate\nevents: [before_command]\ncommand: 'exit 7'\ntimeout_sec: 3\n",
            None,
        )
        .unwrap();
    service.dispatch(Request { method: "POST".into(), path: "/api/hooks/activation".into(), body: json!({"workspace":workspace.path,"path":path,"hash":workspace.read(path).unwrap().hash,"enabled":true}) }).await.unwrap();
    let gated = tools(&service);
    let result = call(
        &gated,
        "background_start",
        json!({"name":"blocked","command":"touch unexpected"}),
    )
    .await;
    assert!(
        !result.success && result.error.contains("blocked by lifecycle"),
        "{}",
        result.error
    );
    assert!(!workspace.path.join("unexpected").exists());
    assert_eq!(
        service
            .engine
            .background()
            .list(&workspace.path)
            .unwrap()
            .len(),
        1
    );
    service.engine.shutdown().await.unwrap();
}

fn response(text: &str, calls: Value) -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":if calls.as_array().unwrap().is_empty(){"stop"}else{"tool_calls"}}]})
}
fn tool(name: &str, args: Value) -> Value {
    json!({"id":shadowcode_core::id(),"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn latest_result(body: &Value) -> Value {
    serde_json::from_str(
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "tool")
            .unwrap()["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn model_tasks_share_project_processes_after_completion_and_cancellation() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let names: Vec<_> = body["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap()).collect();
        let (value, delay) = match index {
            0 | 5 => {
                assert!(names.contains(&"background_start") && names.contains(&"background_stop"));
                (response("Starting watcher", json!([tool("background_start", json!({"name":format!("watch-{index}"),"command":"printf READY; sleep 60"}))])), Duration::ZERO)
            }
            1 => {
                let result = latest_result(body);
                assert_eq!(result["success"], true);
                (response("Checking status", json!([tool("background_output", json!({"id":result["output"]["id"]}))])), Duration::ZERO)
            }
            2 => (response("The watcher is registered; readiness has not been independently verified.", json!([])), Duration::ZERO),
            3 => {
                assert!(!names.contains(&"background_start") && !names.contains(&"background_stop"));
                assert!(names.contains(&"background_list") && names.contains(&"background_output"));
                (response("Inspecting managed processes", json!([tool("background_list", json!({}))])), Duration::ZERO)
            }
            4 => {
                let result = latest_result(body);
                assert_eq!(result["success"], true);
                assert_eq!(result["output"]["tasks"].as_array().unwrap().len(), 1);
                (response("The project has one managed watcher.", json!([])), Duration::ZERO)
            }
            6 => {
                assert_eq!(latest_result(body)["success"], true);
                (response("This response should be cancelled", json!([])), Duration::from_secs(30))
            }
            _ => panic!("unexpected request {index}"),
        };
        (value, delay)
    }).await;
    let (_root, service) = fixture(&server.endpoint);
    let request = |task: &str, mode: &str| StartRequest {
        workspace: service.workspace().unwrap(),
        task: task.into(),
        session_id: None,
        model: None,
        mode: mode.into(),
        queue: false,
        images: Vec::new(),
        web: false,
    };
    let job = service
        .engine
        .start(request("Start a project watcher", "code"))
        .await
        .unwrap();
    let approval = pending(&service).await;
    service
        .engine
        .approvals()
        .decide(&approval.id, &approval.session_id, true)
        .unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(8), service.engine.wait(&job.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, "completed", "{}", completed.summary);
    let first = service
        .engine
        .background()
        .list(&service.workspace().unwrap())
        .unwrap()
        .pop()
        .unwrap();
    let live = until(&service, &first.id, |t| t.output == "READY").await;
    assert!(!dead(live.pid));
    let panel = service
        .dispatch(Request {
            method: "GET".into(),
            path: "/api/background".into(),
            body: Value::Null,
        })
        .await
        .unwrap();
    assert!(panel.to_string().contains(&first.id));
    let review = service
        .engine
        .start(request(
            "Inspect this project's managed background processes",
            "review",
        ))
        .await
        .unwrap();
    let reviewed = tokio::time::timeout(Duration::from_secs(8), service.engine.wait(&review.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reviewed.status, "completed", "{}", reviewed.summary);
    let interrupted = service
        .engine
        .start(request("Start another project watcher", "code"))
        .await
        .unwrap();
    let approval = pending(&service).await;
    service
        .engine
        .approvals()
        .decide(&approval.id, &approval.session_id, true)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while server.requests.lock().unwrap().len() < 7 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let second = service
        .engine
        .background()
        .list(&service.workspace().unwrap())
        .unwrap()
        .into_iter()
        .find(|t| t.id != first.id)
        .unwrap();
    let second = until(&service, &second.id, |t| t.output == "READY").await;
    assert_eq!(
        service.engine.cancel(&interrupted.id).await.unwrap().status,
        "cancelled"
    );
    assert!(!dead(second.pid) && !dead(live.pid));
    assert_eq!(
        service.engine.background().get(&second.id).unwrap().status,
        "RUNNING"
    );
    service.engine.shutdown().await.unwrap();
    assert!(dead(live.pid) && dead(second.pid));
    assert_eq!(server.requests.lock().unwrap().len(), 7);
}

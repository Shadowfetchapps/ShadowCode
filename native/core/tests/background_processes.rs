mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    background::BackgroundTask,
    config::{Config, PermissionLevel},
    engine::StartRequest,
    paths::AppPaths,
    process::{self, ProcessMonitor, ProcessSpec},
    service::{Request, Service},
};
use std::{
    fs,
    process::{Command, Stdio},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

fn setup(trusted: bool) -> (tempfile::TempDir, Service) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({"trusted_workspaces":if trusted{vec![project.clone()]}else{vec![]}}),
    )
    .unwrap();
    (root, Service::open(paths, Some(project)).unwrap())
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
async fn start(service: &Service, name: &str, command: &str) -> BackgroundTask {
    serde_json::from_value(
        call(
            service,
            "POST",
            "/api/background",
            json!({"name":name,"command":command}),
        )
        .await
        .unwrap(),
    )
    .unwrap()
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
    .expect("background state timed out")
}
fn dead(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z') || rest.starts_with('X')),
        Err(_) => true,
    }
}

#[tokio::test]
async fn background_commands_require_trust_and_obey_terminal_permission_denials() {
    let (_root, service) = setup(false);
    let body = json!({"name":"watch","command":"touch unexpected"});
    assert!(call(&service, "POST", "/api/background", body.clone())
        .await
        .unwrap_err()
        .to_string()
        .contains("Trust"));
    Config::patch(service.engine.paths(),json!({"trusted_workspaces":[service.workspace().unwrap()],"permissions":{"level":"read_only"}})).unwrap();
    assert!(call(&service, "POST", "/api/background", body.clone())
        .await
        .unwrap_err()
        .to_string()
        .contains("read-only"));
    Config::patch(
        service.engine.paths(),
        json!({"permissions":{"level":"workspace","network":false}}),
    )
    .unwrap();
    for command in ["npm run dev", "sudo true"] {
        assert!(call(
            &service,
            "POST",
            "/api/background",
            json!({"name":"watch","command":command})
        )
        .await
        .is_err());
    }
    assert!(!service.workspace().unwrap().join("unexpected").exists());
    assert!(service
        .engine
        .background()
        .list(&service.workspace().unwrap())
        .unwrap()
        .is_empty());
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn live_output_handles_split_utf8_and_background_lifetime_has_no_foreground_timeout() {
    let root = tempfile::tempdir().unwrap();
    let monitor = ProcessMonitor::default();
    let result=process::run_background(ProcessSpec::shell("printf '\\360\\237'; sleep 0.08; printf '\\230\\200'; printf ' stderr' >&2; sleep 0.08; printf ' finished'",root.path().into(),Duration::from_millis(1)),CancellationToken::new(),monitor.clone()).await.unwrap();
    assert!(result.ok);
    let live = monitor.snapshot().unwrap();
    assert!(live.pid > 0);
    assert!(live.output.contains('😀'), "{}", live.output);
    assert!(!live.output.contains('\u{fffd}'));
    assert!(live.output.contains("stderr") && live.output.contains("finished"));
    assert!(!live.truncated);
}

#[tokio::test]
async fn output_flood_keeps_a_live_bounded_tail_and_stop_preserves_the_cancelled_result() {
    let (_root, service) = setup(true);
    let task = start(
        &service,
        "flood",
        "head -c 400000 /dev/zero | tr '\\000' x; printf 'LATEST-OUTPUT'; sleep 30",
    )
    .await;
    let live = until(&service, &task.id, |task| {
        task.output.ends_with("LATEST-OUTPUT")
    })
    .await;
    assert_eq!(live.status, "RUNNING");
    assert!(live.pid > 0);
    assert!(live.truncated && live.output.len() <= 64000);
    let preview = call(&service, "GET", "/api/background", Value::Null)
        .await
        .unwrap();
    assert!(preview["tasks"][0]["output"].as_str().unwrap().len() <= 4000);
    assert_eq!(preview["tasks"][0]["output_preview_truncated"], true);
    let full = call(
        &service,
        "GET",
        &format!("/api/background/{}", task.id),
        Value::Null,
    )
    .await
    .unwrap();
    assert!(full["output"].as_str().unwrap().len() > 4000);
    let stopped = service.engine.background().stop(&task.id).await.unwrap();
    assert_eq!(stopped.status, "CANCELLED");
    assert!(stopped.output.ends_with("LATEST-OUTPUT"));
    assert!(dead(stopped.pid));
    assert_eq!(
        service
            .engine
            .background()
            .stop(&task.id)
            .await
            .unwrap()
            .status,
        "CANCELLED"
    );
    let stored = service
        .engine
        .store()
        .background_task(&task.id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, "CANCELLED");
    assert!(stored.output.len() <= 64000);
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn completion_failure_graceful_stop_and_forced_child_cleanup_are_truthful() {
    let (_root, service) = setup(true);
    for (name, command, expected, exit) in [
        ("ok", "printf complete", "COMPLETED", 0),
        ("failure", "printf broken >&2; exit 7", "FAILED", 7),
    ] {
        let task = start(&service, name, command).await;
        let done = until(&service, &task.id, |task| task.status == expected).await;
        assert_eq!(done.exit_code, Some(exit));
        assert_eq!(
            service
                .engine
                .background()
                .stop(&task.id)
                .await
                .unwrap()
                .status,
            expected
        );
    }
    let graceful = start(
        &service,
        "graceful",
        "trap 'printf graceful-stop; exit 0' TERM; printf ready; while :; do sleep 0.1; done",
    )
    .await;
    until(&service, &graceful.id, |task| task.output.contains("ready")).await;
    let stopped = service
        .engine
        .background()
        .stop(&graceful.id)
        .await
        .unwrap();
    assert_eq!(stopped.status, "CANCELLED");
    assert!(stopped.output.contains("graceful-stop"));
    let task = start(
        &service,
        "stubborn",
        "trap '' TERM; sleep 30 & echo $! > child.pid; printf child-ready; wait",
    )
    .await;
    until(&service, &task.id, |task| {
        task.output.contains("child-ready")
    })
    .await;
    let child = fs::read_to_string(service.workspace().unwrap().join("child.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let stopped = tokio::time::timeout(
        Duration::from_secs(6),
        service.engine.background().stop(&task.id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stopped.status, "CANCELLED");
    assert!(dead(child));
    assert!(dead(stopped.pid));
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn workspace_scope_duplicate_names_and_active_limits_are_enforced() {
    let (root, service) = setup(true);
    let first = start(&service, "dev", "sleep 30").await;
    assert!(call(
        &service,
        "POST",
        "/api/background",
        json!({"name":"dev","command":"sleep 30"})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("already running"));
    for index in 1..4 {
        start(&service, &format!("watch-{index}"), "sleep 30").await;
    }
    assert!(call(
        &service,
        "POST",
        "/api/background",
        json!({"name":"fifth","command":"sleep 30"})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("four"));
    let other = root.path().join("other");
    fs::create_dir(&other).unwrap();
    call(
        &service,
        "POST",
        "/api/projects/trust",
        json!({"path":other}),
    )
    .await
    .unwrap();
    assert!(call(&service, "GET", "/api/background", Value::Null)
        .await
        .unwrap()["tasks"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(call(
        &service,
        "POST",
        &format!("/api/background/{}/stop", first.id),
        json!({})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("Switch"));
    let task = start(&service, "dev", "sleep 30").await;
    until(&service, &task.id, |task| task.pid > 0).await;
    service.engine.shutdown().await.unwrap();
    assert_eq!(
        service.engine.background().get(&task.id).unwrap().status,
        "CANCELLED"
    );
    assert_eq!(
        service.engine.background().get(&first.id).unwrap().status,
        "CANCELLED"
    );
    assert!(call(
        &service,
        "POST",
        "/api/background",
        json!({"name":"late","command":"sleep 30"})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("shutting down"));
}

#[tokio::test]
async fn background_work_coexists_with_agent_tasks_and_shutdown_cleans_both() {
    let server=support::server(|_,_|(json!({"choices":[{"message":{"role":"assistant","content":"Project inspected"},"finish_reason":"stop"}]}),Duration::ZERO)).await;
    let (_root, service) = setup(true);
    Config::patch(
        service.engine.paths(),
        json!({"model":{"provider":"local","endpoint":server.endpoint,"name":"fixture"}}),
    )
    .unwrap();
    let process = start(&service, "server", "sleep 30").await;
    until(&service, &process.id, |task| task.pid > 0).await;
    let job = service
        .engine
        .start(StartRequest {
            workspace: service.workspace().unwrap(),
            task: "Explain the project".into(),
            session_id: None,
            model: None,
            mode: "plan".into(),
            queue: false,
            images: Vec::new(),
            web: false,
        })
        .await
        .unwrap();
    assert_eq!(
        service.engine.wait(&job.id).await.unwrap().status,
        "completed"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert_eq!(
        service.engine.background().get(&process.id).unwrap().status,
        "RUNNING"
    );
    service.engine.shutdown().await.unwrap();
    assert!(dead(
        service.engine.background().get(&process.id).unwrap().pid
    ));
}

#[tokio::test]
async fn legacy_import_and_native_restart_preserve_history_without_signalling_old_pids() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let legacy = paths.state.join("background.db");
    let mut unrelated = Command::new("sleep")
        .arg("30")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let db = rusqlite::Connection::open(&legacy).unwrap();
    db.execute_batch("CREATE TABLE tasks(id TEXT,name TEXT,command TEXT,cwd TEXT,status TEXT,pid INTEGER,started_at REAL,ended_at REAL,exit_code INTEGER,output TEXT,error TEXT)").unwrap();
    db.execute(
        "INSERT INTO tasks VALUES('old','old server','sleep 30',?,'RUNNING',?,1,NULL,NULL,?,'')",
        rusqlite::params![
            project.to_str().unwrap(),
            unrelated.id(),
            format!("{}tail", "🦀".repeat(30000))
        ],
    )
    .unwrap();
    drop(db);
    let original = fs::read(&legacy).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    let task = service.engine.background().get("legacy:old").unwrap();
    assert_eq!(task.status, "INTERRUPTED");
    assert!(task.output.ends_with("tail"));
    assert!(task.output.len() <= 64000);
    service.engine.background().stop(&task.id).await.unwrap();
    assert!(unrelated.try_wait().unwrap().is_none());
    let fake = BackgroundTask {
        id: "native-interrupted".into(),
        name: "stale native".into(),
        command: "sleep 30".into(),
        cwd: project.to_string_lossy().into_owned(),
        status: "RUNNING".into(),
        pid: unrelated.id(),
        ..Default::default()
    };
    service.engine.store().save_background(&fake).unwrap();
    service.engine.shutdown().await.unwrap();
    drop(service);
    let reopened = Service::open(paths, Some(project.clone())).unwrap();
    assert_eq!(
        reopened.engine.background().get(&fake.id).unwrap().status,
        "INTERRUPTED"
    );
    reopened.engine.background().stop(&fake.id).await.unwrap();
    assert!(unrelated.try_wait().unwrap().is_none());
    assert_eq!(
        reopened.engine.background().list(&project).unwrap().len(),
        2
    );
    assert_eq!(fs::read(legacy).unwrap(), original);
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    reopened.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropping_the_manager_cancels_processes_and_holds_the_profile_lock_until_cleanup() {
    let (root, service) = setup(true);
    let task = start(&service, "owned", "sleep 30").await;
    let live = until(&service, &task.id, |task| task.pid > 0).await;
    drop(service);
    tokio::time::timeout(Duration::from_secs(8), async {
        while !dead(live.pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let reopened = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(service) = Service::open(
                AppPaths::isolated(&root.path().join("profile")).unwrap(),
                Some(root.path().join("project")),
            ) {
                break service;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        reopened.engine.background().get(&task.id).unwrap().status,
        "CANCELLED"
    );
    reopened.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn global_limit_and_concurrent_stop_calls_keep_process_ownership_bounded() {
    let (root, service) = setup(true);
    let mut config = Config::load(service.engine.paths(), None).unwrap();
    config.permissions.level = PermissionLevel::Workspace;
    let mut tasks = Vec::new();
    for index in 0..5 {
        let project = root.path().join(format!("project-{index}"));
        fs::create_dir(&project).unwrap();
        config
            .trusted_workspaces
            .push(project.to_string_lossy().into_owned());
        for count in 0..4 {
            let task = service.engine.background().start(
                &project,
                &config,
                None,
                &format!("task-{count}"),
                "sleep 30",
            );
            if index == 4 {
                assert!(task.unwrap_err().to_string().contains("16"));
            } else {
                tasks.push(task.unwrap());
            }
        }
    }
    let (one, two) = tokio::join!(
        service.engine.background().stop(&tasks[0].id),
        service.engine.background().stop(&tasks[0].id)
    );
    assert_eq!(one.unwrap().status, "CANCELLED");
    assert_eq!(two.unwrap().status, "CANCELLED");
    service.engine.shutdown().await.unwrap();
    for task in tasks {
        let task = service.engine.background().get(&task.id).unwrap();
        if task.pid != 0 {
            assert!(dead(task.pid));
        }
    }
}

#[tokio::test]
async fn long_running_processes_remain_visible_beyond_the_recent_history_page() {
    let (_root, service) = setup(true);
    let active = start(&service, "long-running", "sleep 30").await;
    for index in 0..105 {
        service
            .engine
            .store()
            .save_background(&BackgroundTask {
                id: format!("historical-{index}"),
                name: "completed check".into(),
                cwd: active.cwd.clone(),
                status: "COMPLETED".into(),
                started_at: active.started_at + index as f64 + 1.0,
                ..Default::default()
            })
            .unwrap();
    }
    let tasks = service
        .engine
        .background()
        .list(&service.workspace().unwrap())
        .unwrap();
    assert_eq!(tasks.len(), 101);
    assert!(tasks.iter().any(|task| task.id == active.id));
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_audit_storage_never_starts_an_unrecorded_process() {
    let (_root, service) = setup(true);
    let db = rusqlite::Connection::open(service.engine.paths().database()).unwrap();
    db.execute_batch("CREATE TRIGGER deny_background_audit BEFORE INSERT ON events WHEN NEW.type='background.started' BEGIN SELECT RAISE(ABORT,'audit unavailable'); END;").unwrap();
    let result = call(
        &service,
        "POST",
        "/api/background",
        json!({"name":"unrecorded","command":"touch should-not-exist"}),
    )
    .await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("audit unavailable"));
    assert!(service
        .engine
        .background()
        .list(&service.workspace().unwrap())
        .unwrap()
        .is_empty());
    assert!(!service
        .workspace()
        .unwrap()
        .join("should-not-exist")
        .exists());
    service.engine.shutdown().await.unwrap();
}

/// A progress save that fails (the database is briefly unwritable) never
/// stops the user's running process; saving resumes when it can.
#[tokio::test]
async fn a_failed_progress_save_does_not_kill_the_process() {
    let (_root, service) = setup(true);
    let task = start(
        &service,
        "ticker",
        "while true; do echo tick; sleep 0.1; done",
    )
    .await;
    let running = until(&service, &task.id, |t| t.status == "RUNNING" && t.pid > 0).await;
    let pid = running.pid;
    let db = rusqlite::Connection::open(service.engine.paths().database()).unwrap();
    db.busy_timeout(Duration::from_secs(10)).unwrap();
    db.execute_batch("CREATE TRIGGER deny_progress BEFORE UPDATE ON background_processes BEGIN SELECT RAISE(ABORT,'database briefly unavailable'); END;").unwrap();
    // Several progress ticks fail meanwhile.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!dead(pid), "the process was killed by a failed save");
    assert_eq!(
        service.engine.background().get(&task.id).unwrap().status,
        "RUNNING"
    );
    db.execute_batch("DROP TRIGGER deny_progress;").unwrap();
    call(
        &service,
        "POST",
        &format!("/api/background/{}/stop", task.id),
        json!({}),
    )
    .await
    .unwrap();
    let stopped = until(&service, &task.id, |t| t.status != "RUNNING").await;
    assert_eq!(stopped.status, "CANCELLED");
    service.engine.shutdown().await.unwrap();
}

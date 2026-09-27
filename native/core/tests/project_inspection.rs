mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    project,
    service::{Request, Service},
    workspace::Workspace,
};
use std::{fs, os::unix::fs::symlink, path::Path, sync::Arc, time::Duration};
fn fixture() -> (tempfile::TempDir, Service) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("project");
    fs::create_dir(&path).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths,json!({"trusted_workspaces":[path],"model":{"provider":"mock"},"permissions":{"approve_shell":false}})).unwrap();
    (root, Service::open(paths, Some(path)).unwrap())
}
async fn api(service: &Service, method: &str, path: &str, body: Value) -> anyhow::Result<Value> {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
}
async fn pending(service: &Service) -> shadowcode_core::approvals::Approval {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if let Some(value) = service.engine.approvals().list(None).into_iter().next() {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
async fn wait(service: &Service, job: &Value) -> Value {
    json!(tokio::time::timeout(
        Duration::from_secs(5),
        service.engine.wait(job["id"].as_str().unwrap())
    )
    .await
    .unwrap()
    .unwrap())
}
#[tokio::test]
async fn project_scan_is_bounded_confined_and_explicit_about_ambiguous_tests() {
    let (root, service) = fixture();
    let path = service.workspace().unwrap();
    fs::create_dir_all(path.join("src")).unwrap();
    fs::create_dir_all(path.join("ui/src")).unwrap();
    fs::create_dir_all(path.join("node_modules/package")).unwrap();
    fs::write(path.join("Cargo.toml"), "[package]\nname='fixture'\n").unwrap();
    fs::write(
        path.join("package.json"),
        r#"{"scripts":{"test":"exit 99"}}"#,
    )
    .unwrap();
    fs::write(
        path.join("ui/package.json"),
        r#"{"scripts":{"test":"echo tests"},"dependencies":{"react":"1"}}"#,
    )
    .unwrap();
    fs::write(
        path.join("src/lib.rs"),
        format!("// TODO inspect\n{}", "x".repeat(150_000)),
    )
    .unwrap();
    fs::write(path.join("ui/src/index.tsx"), "// FIXME later\n").unwrap();
    fs::write(
        path.join("node_modules/package/ignored.py"),
        "HACK private-dependency",
    )
    .unwrap();
    fs::write(root.path().join("secret.py"), "outside-private-data").unwrap();
    symlink(root.path().join("secret.py"), path.join("leak.py")).unwrap();
    let map = project::inspect(Arc::new(Workspace::open(&path).unwrap()))
        .await
        .unwrap();
    assert_eq!(map["inspection"]["source_files"], 2);
    assert_eq!(map["inspection"]["truncated"], true);
    assert!(map["stack"]["frameworks"]
        .as_array()
        .unwrap()
        .contains(&json!("react")));
    assert!(!map.to_string().contains("outside-private-data"));
    assert!(!map.to_string().contains("private-dependency"));
    assert!(project::test_command(&map)
        .unwrap_err()
        .to_string()
        .contains("2 candidates"));
    fs::remove_file(path.join("package.json")).unwrap();
    assert_eq!(
        project::test_command(
            &project::inspect(Arc::new(Workspace::open(&path).unwrap()))
                .await
                .unwrap()
        )
        .unwrap(),
        "cargo test"
    );
    let deep = path.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q");
    fs::remove_file(path.join("src/lib.rs")).unwrap();
    fs::create_dir_all(deep).unwrap();
    assert_eq!(
        project::inspect(Arc::new(Workspace::open(&path).unwrap()))
            .await
            .unwrap()["inspection"]["truncated"],
        true
    );
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn project_scan_caps_aggregate_reads_and_skips_special_files() {
    use std::os::unix::ffi::OsStrExt;
    let (_root, service) = fixture();
    let path = service.workspace().unwrap();
    fs::create_dir(path.join("src")).unwrap();
    let content = "x".repeat(64_000);
    for i in 0..160 {
        fs::write(path.join(format!("src/file-{i:03}.rs")), &content).unwrap();
    }
    let fifo = std::ffi::CString::new(path.join("blocked.py").as_os_str().as_bytes()).unwrap();
    // SAFETY: the CString is a valid, NUL-terminated path in this disposable fixture.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let map = tokio::time::timeout(
        Duration::from_secs(5),
        project::inspect(Arc::new(Workspace::open(&path).unwrap())),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(map["inspection"]["source_files"], 160);
    assert_eq!(map["inspection"]["bytes_read"], 8_000_000);
    assert_eq!(map["inspection"]["truncated"], true);
    service.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn saving_project_maps_preserves_notes_and_requires_trust_and_write_access() {
    let (_root, service) = fixture();
    let path = service.workspace().unwrap();
    fs::create_dir_all(path.join(".shadow/memory")).unwrap();
    let notes = path.join(".shadow/memory/project.md");
    fs::write(&notes, "My existing project notes.\n").unwrap();
    let first = api(
        &service,
        "POST",
        "/api/workspace/understand",
        json!({"save":true}),
    )
    .await
    .unwrap();
    assert_eq!(first["saved"], true);
    fs::write(path.join("go.mod"), "module fixture\n").unwrap();
    api(
        &service,
        "POST",
        "/api/workspace/understand",
        json!({"save":true}),
    )
    .await
    .unwrap();
    let text = fs::read_to_string(&notes).unwrap();
    assert!(text.starts_with("My existing project notes."));
    assert_eq!(
        text.matches("<!-- shadowcode:project-map:start -->")
            .count(),
        1
    );
    assert!(text.contains("go test"));
    Config::patch(
        service.engine.paths(),
        json!({"permissions":{"level":"read_only"}}),
    )
    .unwrap();
    assert!(api(
        &service,
        "POST",
        "/api/workspace/understand",
        json!({"save":true})
    )
    .await
    .is_err());
    assert_eq!(
        api(&service, "GET", "/api/workspace/understand", Value::Null)
            .await
            .unwrap()["saved"],
        false
    );
    assert_eq!(fs::read_to_string(&notes).unwrap(), text);
    assert!(project::merge_memory("<!-- shadowcode:project-map:start -->", "next").is_err());
    assert!(project::merge_memory(&"x".repeat(16_000), "new map").is_err());
    service.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn native_doctor_reports_real_checks_without_implicit_model_or_shell_execution() {
    let model = support::server(|_, _| {
        (
            json!({"choices":[{"message":{"content":"connected"},"finish_reason":"stop"}]}),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, service) = fixture();
    Config::patch(service.engine.paths(),json!({"model":{"provider":"local","endpoint":model.endpoint,"name":"fixture","default":"fixture"}})).unwrap();
    let report = api(&service, "GET", "/api/doctor", Value::Null)
        .await
        .unwrap();
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "database" && c["status"] == "pass"));
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "model-response" && c["status"] == "not_checked"));
    assert!(!report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "python" || c["id"] == "port"));
    let snapshot = &report["diagnostic_export"];
    let content = snapshot["content"].as_str().unwrap();
    let exported: Value = serde_json::from_str(content).unwrap();
    assert_eq!(snapshot["byte_length"], content.len());
    assert_eq!(exported["schema"], 1);
    assert_eq!(
        exported["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "model-response")
            .unwrap()["status"],
        "not_checked"
    );
    assert!(!content.contains(&service.workspace().unwrap().display().to_string()));
    assert!(!content.contains(&model.endpoint));
    assert!(!content.contains("fixture"));
    assert!(exported.get("project_map").is_none());
    let id = snapshot["id"].as_str().unwrap();
    let retained = api(
        &service,
        "GET",
        &format!("/api/diagnostic-exports/{id}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(retained["content"], snapshot["content"]);
    let mut remote = retained.clone();
    shadowcode_core::remote::policy::redact_response(&mut remote);
    assert_eq!(remote["content"], snapshot["content"]);
    for id in ["cli-codex", "cli-grok", "cli-claude"] {
        assert!(
            report["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["id"] == id && c["status"] != "fail"),
            "doctor missing {id}"
        );
    }
    assert!(model.requests.lock().unwrap().is_empty());
    let tested = api(&service, "GET", "/api/doctor?test_model=true", Value::Null)
        .await
        .unwrap();
    assert!(tested["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "model-response" && c["status"] == "pass"));
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    fs::write(
        service.engine.paths().secrets_file(),
        "PRIVATE_TOKEN=never-in-report",
    )
    .unwrap();
    let report = api(&service, "GET", "/api/doctor", Value::Null)
        .await
        .unwrap();
    assert!(!report.to_string().contains("never-in-report"));
    service.engine.shutdown().await.unwrap();
}
fn git(path: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
#[tokio::test]
async fn change_history_uses_literal_paths_and_disables_external_diff_helpers() {
    let (_root, service) = fixture();
    let path = service.workspace().unwrap();
    git(&path, &["init", "-q"]);
    assert_eq!(
        api(&service, "GET", "/api/workspace/why", Value::Null)
            .await
            .unwrap()["empty_history"],
        true
    );
    git(&path, &["config", "user.name", "Fixture"]);
    git(&path, &["config", "user.email", "fixture@example.test"]);
    let name = ":(glob)*.txt";
    fs::write(path.join(name), "original\n").unwrap();
    fs::write(path.join("other.txt"), "unrelated\n").unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "-qm", "Original fixtures"]);
    fs::write(path.join(name), "updated\n").unwrap();
    git(
        &path,
        &["config", "diff.external", "touch unexpected-helper"],
    );
    let url = format!(
        "/api/workspace/why?path={}",
        reqwest::Url::parse_with_params("http://local", &[("path", name)])
            .unwrap()
            .query()
            .unwrap()
            .strip_prefix("path=")
            .unwrap()
    );
    let report = api(&service, "GET", &url, Value::Null).await.unwrap();
    assert!(report["log"]
        .as_str()
        .unwrap()
        .contains("Original fixtures"));
    assert!(report["diff"]["diff"].as_str().unwrap().contains("updated"));
    let card = api(
        &service,
        "POST",
        "/api/commands/run",
        json!({"name":"why","args":name}),
    )
    .await
    .unwrap();
    let body = card["body"].as_str().unwrap();
    assert!(
        body.contains("Original fixtures")
            && body.contains("Working tree:")
            && body.contains("updated")
            && body.contains("Staged:"),
        "{card}"
    );
    assert!(!path.join("unexpected-helper").exists());
    git(
        &path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "inspection-link",
            "../linked",
        ],
    );
    let linked = service
        .fork_selection(path.parent().unwrap().join("linked"), None)
        .unwrap();
    assert_eq!(
        api(&linked, "GET", "/api/workspace/why", Value::Null)
            .await
            .unwrap()["empty_history"],
        false
    );
    assert!(api(
        &service,
        "GET",
        "/api/workspace/why?path=../outside",
        Value::Null
    )
    .await
    .is_err());
    service.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn native_test_jobs_need_exact_approval_and_preserve_output_and_failure_without_a_model() {
    let (_root, service) = fixture();
    let path = service.workspace().unwrap();
    for (command, allow, status, code) in [
        ("printf native-test", true, "completed", Some(0)),
        ("printf failure-output; exit 7", true, "failed", Some(7)),
        ("touch denied-test", false, "failed", None),
    ] {
        let job = api(
            &service,
            "POST",
            "/api/jobs/test",
            json!({"command":command}),
        )
        .await
        .unwrap();
        let approval = pending(&service).await;
        assert_eq!(approval.command, command);
        assert_eq!(approval.task_id, job["task_id"]);
        assert!(!path.join("denied-test").exists());
        service
            .engine
            .approvals()
            .decide(&approval.id, &approval.session_id, allow)
            .unwrap();
        let done = wait(&service, &job).await;
        assert_eq!(done["status"], status, "{done}");
        assert_eq!(done["result"]["command"]["exit_code"], json!(code));
        assert_eq!(done["usage"]["total_tokens"], 0);
        if code == Some(7) {
            assert_eq!(done["result"]["command"]["stdout"], "failure-output");
        }
    }
    assert!(!path.join("denied-test").exists());
    Config::patch(
        service.engine.paths(),
        json!({"permissions":{"level":"read_only"}}),
    )
    .unwrap();
    assert!(api(
        &service,
        "POST",
        "/api/jobs/test",
        json!({"command":"touch forbidden"})
    )
    .await
    .is_err());
    service.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn command_completion_hooks_run_once_and_cannot_hide_test_output_or_failure() {
    let (_root, service) = fixture();
    let path = service.workspace().unwrap();
    let hook = ".shadowcode/hooks/completion.yaml";
    fs::create_dir_all(path.join(".shadowcode/hooks")).unwrap();
    for (exit, status) in [(0, "completed"), (9, "failed")] {
        fs::write(
            path.join(hook),
            serde_yaml_ng::to_string(&json!({
                "name":"completion", "events":["on_complete"],
                "command":format!("printf x >> hook-runs; exit {exit}"), "timeout_sec":3
            }))
            .unwrap(),
        )
        .unwrap();
        let hash = Workspace::open(&path).unwrap().read(hook).unwrap().hash;
        api(
            &service,
            "POST",
            "/api/hooks/activation",
            json!({
                "workspace":path,"path":hook,"hash":hash,"enabled":true
            }),
        )
        .await
        .unwrap();
        let job = api(
            &service,
            "POST",
            "/api/jobs/test",
            json!({"command":"printf retained-test-output"}),
        )
        .await
        .unwrap();
        let approval = pending(&service).await;
        service
            .engine
            .approvals()
            .decide(&approval.id, &approval.session_id, true)
            .unwrap();
        let done = wait(&service, &job).await;
        assert_eq!(done["status"], status, "{done}");
        assert_eq!(done["result"]["command"]["stdout"], "retained-test-output");
        assert_eq!(done["result"]["command"]["exit_code"], 0);
        assert_eq!(done["result"]["command"]["success"], exit == 0);
        assert_eq!(done["usage"]["total_tokens"], 0);
        if exit != 0 {
            assert!(done["result"]["command"]["error"]
                .as_str()
                .unwrap()
                .contains("Completion checks failed"));
        }
    }
    assert_eq!(fs::read_to_string(path.join("hook-runs")).unwrap(), "xx");
    service.engine.shutdown().await.unwrap();
}

// A sandbox child reports a namespace-local PID. Resolve its host identity
// while it is alive, then assert that exact process is reaped on shutdown.
fn host_pid(project: &std::path::Path, namespace_pid: u32) -> u32 {
    let project = project.canonicalize().unwrap();
    let matches: Vec<_> = fs::read_dir("/proc")
        .unwrap()
        .filter_map(|e| {
            let path = e.ok()?.path();
            let pid = path.file_name()?.to_str()?.parse::<u32>().ok()?;
            if fs::read_link(path.join("cwd")).ok()? != project {
                return None;
            }
            let status = fs::read_to_string(path.join("status")).ok()?;
            let inner = status
                .lines()
                .find(|line| line.starts_with("NSpid:"))?
                .split_whitespace()
                .last()?
                .parse::<u32>()
                .ok()?;
            (inner == namespace_pid).then_some(pid)
        })
        .collect();
    assert_eq!(matches.len(), 1, "Expected exactly one live fixture child");
    matches[0]
}
#[tokio::test]
async fn test_timeouts_stop_command_children_and_keep_the_workspace_reusable() {
    let (_root, service) = fixture();
    let path = service.workspace().unwrap();
    let job = api(
        &service,
        "POST",
        "/api/jobs/test",
        json!({"command":"sleep 60 & echo $! > test-child.pid; wait","timeout":1}),
    )
    .await
    .unwrap();
    let approval = pending(&service).await;
    service
        .engine
        .approvals()
        .decide(&approval.id, &approval.session_id, true)
        .unwrap();
    let namespace_pid = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(pid) = fs::read_to_string(path.join("test-child.pid"))
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let pid = host_pid(&path, namespace_pid);
    let done = wait(&service, &job).await;
    assert_eq!(done["status"], "failed");
    assert_eq!(done["result"]["command"]["timed_out"], true);
    assert!(!fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z ")));
    let next = api(
        &service,
        "POST",
        "/api/jobs/test",
        json!({"command":"printf next"}),
    )
    .await
    .unwrap();
    let approval = pending(&service).await;
    service
        .engine
        .approvals()
        .decide(&approval.id, &approval.session_id, true)
        .unwrap();
    assert_eq!(wait(&service, &next).await["status"], "completed");
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_test_job_records_one_prompt_in_live_feed_and_reopen_for_each_invocation() {
    let (_root, service) = fixture();
    let project = service.workspace().unwrap();
    let command = "printf x >> command-invocations; printf command-output";
    let expected = format!("Run test command: {command}");
    let mut session = None;
    let mut task_ids = Vec::new();
    let mut live_pages = Vec::new();
    let mut done_pages = Vec::new();
    for _ in 0..2 {
        let job = api(
            &service,
            "POST",
            "/api/jobs/test",
            json!({
                "command":command,"session_id":session,"timeout":5
            }),
        )
        .await
        .unwrap();
        let approval = pending(&service).await;
        assert_eq!(approval.command, command);
        assert_eq!(approval.task_id, job["task_id"]);
        let feed = format!(
            "/api/jobs/{}/events?after={}&limit=512",
            job["id"].as_str().unwrap(),
            job["event_cursor"].as_i64().unwrap()
        );
        // This is the same durable read path used by nativeJobStream while
        // approval is pending; no command has yet been permitted for this job.
        live_pages.push(api(&service, "GET", &feed, Value::Null).await.unwrap());
        service
            .engine
            .approvals()
            .decide(&approval.id, &approval.session_id, true)
            .unwrap();
        let done = wait(&service, &job).await;
        assert_eq!(done["status"], "completed", "{done}");
        assert_eq!(done["result"]["command"]["stdout"], "command-output");
        assert_eq!(done["usage"]["total_tokens"], 0);
        done_pages.push(api(&service, "GET", &feed, Value::Null).await.unwrap());
        session = Some(job["session_id"].as_str().unwrap().to_owned());
        task_ids.push(job["task_id"].as_str().unwrap().to_owned());
    }
    assert_eq!(
        fs::read_to_string(project.join("command-invocations")).unwrap(),
        "xx",
        "two explicit requests execute once each; duplicate prompts are not duplicate execution"
    );
    let session = session.unwrap();
    let persisted = service.engine.store().recent_events(&session, 100).unwrap();
    let paths = service.engine.paths().clone();
    service.engine.shutdown().await.unwrap();
    drop(service);
    let reopened = Service::open(paths, Some(project)).unwrap();
    let replay = api(
        &reopened,
        "GET",
        &format!("/api/sessions/{session}"),
        Value::Null,
    )
    .await
    .unwrap();
    reopened.engine.shutdown().await.unwrap();
    for (i, task_id) in task_ids.iter().enumerate() {
        for (label, events) in [
            (
                "live pending feed",
                live_pages[i]["events"].as_array().unwrap(),
            ),
            ("terminal feed", done_pages[i]["events"].as_array().unwrap()),
            ("durable history", &persisted),
            (
                "reopened session replay",
                replay["events"].as_array().unwrap(),
            ),
        ] {
            let prompts: Vec<_> = events
                .iter()
                .filter(|e| e["task_id"] == *task_id && e["type"] == "user.message")
                .collect();
            assert_eq!(
                prompts.len(),
                1,
                "{label}: one explicit command task must have one prompt: {prompts:?}"
            );
            assert_eq!(prompts[0]["payload"]["text"], expected);
        }
        let own: Vec<_> = persisted
            .iter()
            .filter(|e| e["task_id"] == *task_id)
            .collect();
        for kind in [
            "agent.started",
            "tool.started",
            "tool.completed",
            "command.completed",
            "agent.completed",
        ] {
            assert_eq!(
                own.iter().filter(|e| e["type"] == kind).count(),
                1,
                "{task_id}: {kind}"
            );
        }
        assert!(!own.iter().any(|e| e["type"] == "model.request_metadata"));
    }
}

#[tokio::test]
async fn desktop_explicit_test_slash_command_uses_direct_terminal_without_command_job_prompts() {
    let (_root, service) = fixture();
    let project = service.workspace().unwrap();
    // Same endpoint/name/args used by useTaskActions -> api.runCommand.
    // This desktop action differs from the owned /api/jobs/test route.
    let result = api(
        &service,
        "POST",
        "/api/commands/run",
        json!({
            "name":"test", "args":"printf x >> slash-invocations; printf slash-output"
        }),
    )
    .await
    .unwrap();
    assert_eq!(result["headline"], "Command completed", "{result}");
    assert!(result["body"].as_str().unwrap().contains("slash-output"));
    assert!(result["metadata"]["job"].is_null());
    assert_eq!(
        fs::read_to_string(project.join("slash-invocations")).unwrap(),
        "x"
    );
    assert!(service.engine.store().jobs(100).unwrap().is_empty());
    assert!(service.engine.approvals().list(None).is_empty());
    service.engine.shutdown().await.unwrap();
}
